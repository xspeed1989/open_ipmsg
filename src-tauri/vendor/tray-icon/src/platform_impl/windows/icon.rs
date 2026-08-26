// Copyright 2022-2022 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

// taken from https://github.com/rust-windowing/winit/blob/92fdf5ba85f920262a61cee4590f4a11ad5738d1/src/platform_impl/windows/icon.rs

use std::{ffi::c_void, fmt, io, mem, path::Path, sync::Arc};

use windows_sys::{
    core::PCWSTR,
    Win32::UI::WindowsAndMessaging::{
        DestroyIcon, LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE,
    },
};

use crate::icon::*;

use super::util;

impl RgbaIcon {
    /// 把 RGBA 像素做成「32 位 alpha 图标」（DIB section 颜色位图 + 单色 mask），
    /// 而不是原实现的 CreateIcon 经典图标。
    ///
    /// 原实现的问题：CreateIcon 生成的是「单色 AND mask + 32bpp XOR」的经典图标，
    /// 托盘按 mask 绘制（mask 位=0 的像素被 SRCAND 刷黑再画 XOR），alpha 通道
    /// 只在「32 位 alpha-blended 图标」上生效。结果全 0 像素的透明帧被画成黑块/
    /// 马赛克（见 src-tauri/src/lib.rs 的 TRAY_BLANK），与 Electron/Chromium
    /// 的 nativeImage 行为不一致。
    ///
    /// 这里改用 Electron/Chromium 同一套做法：CreateIconIndirect +
    /// CreateDIBSection（32bpp 顶向下 DIB，直通 alpha），DrawIconEx 对这类
    /// 图标走 AC_SRC_OVER 合成且忽略 mask —— 全 0 像素的帧真正透明。
    fn into_windows_icon(self) -> Result<WinIcon, BadIcon> {
        use windows_sys::Win32::{
            Graphics::Gdi::{
                CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HGDIOBJ,
            },
            UI::WindowsAndMessaging::{CreateIconIndirect, ICONINFO},
        };

        let w = self.width as i32;
        let h = self.height as i32;
        let pixel_count = self.rgba.len() / PIXEL_SIZE;
        let rgba = self.rgba;
        // RGBA → BGRA（straight alpha，与 .ico 格式一致）
        let pixels = unsafe { std::slice::from_raw_parts_mut(rgba.as_ptr() as *mut Pixel, pixel_count) };
        for pixel in pixels.iter_mut() {
            mem::swap(&mut pixel.r, &mut pixel.b);
        }

        let hdc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if hdc.is_null() {
            return Err(BadIcon::OsError(io::Error::last_os_error()));
        }

        // 32bpp 顶向下 DIB section：alpha 通道由 DIB 承载
        let mut color_info: BITMAPINFO = unsafe { mem::zeroed() };
        color_info.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
        color_info.bmiHeader.biWidth = w;
        color_info.bmiHeader.biHeight = -h; // top-down
        color_info.bmiHeader.biPlanes = 1;
        color_info.bmiHeader.biBitCount = 32;
        color_info.bmiHeader.biCompression = BI_RGB;
        let mut color_bits: *mut c_void = std::ptr::null_mut();
        let hbm_color = unsafe {
            CreateDIBSection(
                hdc,
                &color_info,
                DIB_RGB_COLORS,
                &mut color_bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if hbm_color.is_null() {
            unsafe { DeleteDC(hdc) };
            return Err(BadIcon::OsError(io::Error::last_os_error()));
        }
        unsafe {
            std::slice::from_raw_parts_mut(color_bits as *mut u8, rgba.len()).copy_from_slice(&rgba);
        }

        // 单色 mask：alpha=0 的像素挖透明孔（经典路径兜底；alpha 路径下被忽略）
        let stride = (((w + 31) / 32) * 4) as usize; // 1bpp，行按 4 字节对齐
        let mut mask_bits = vec![0u8; stride * h as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) as usize;
                if pixels[idx].a == 0 {
                    // 单色位图：每字节 8 像素，MSB 是行内第一个像素
                    mask_bits[y as usize * stride + (x as usize / 8)] |= 0x80 >> (x % 8);
                }
            }
        }
        let hbm_mask = unsafe { CreateBitmap(w, h, 1, 1, mask_bits.as_ptr() as *const c_void) };
        if hbm_mask.is_null() {
            unsafe {
                DeleteObject(hbm_color as HGDIOBJ);
                DeleteDC(hdc);
            }
            return Err(BadIcon::OsError(io::Error::last_os_error()));
        }

        let icon_info = ICONINFO {
            fIcon: 1, // TRUE
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        let handle = unsafe { CreateIconIndirect(&icon_info) };
        // 位图内容已被图标复制，可立即释放
        unsafe {
            DeleteObject(hbm_color as HGDIOBJ);
            DeleteObject(hbm_mask as HGDIOBJ);
            DeleteDC(hdc);
        }
        if !handle.is_null() {
            Ok(WinIcon::from_handle(handle))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }
}

#[derive(Debug)]
struct RaiiIcon {
    handle: HICON,
}

#[derive(Clone)]
pub(crate) struct WinIcon {
    inner: Arc<RaiiIcon>,
}

unsafe impl Send for WinIcon {}

impl WinIcon {
    pub fn as_raw_handle(&self) -> HICON {
        self.inner.handle
    }

    pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, BadIcon> {
        let rgba_icon = RgbaIcon::from_rgba(rgba, width, height)?;
        rgba_icon.into_windows_icon()
    }

    pub(crate) fn from_handle(handle: HICON) -> Self {
        Self {
            #[allow(clippy::arc_with_non_send_sync)]
            inner: Arc::new(RaiiIcon { handle }),
        }
    }

    pub(crate) fn from_path<P: AsRef<Path>>(
        path: P,
        size: Option<(u32, u32)>,
    ) -> Result<Self, BadIcon> {
        // width / height of 0 along with LR_DEFAULTSIZE tells windows to load the default icon size
        let (width, height) = size.unwrap_or((0, 0));

        let wide_path = util::encode_wide(path.as_ref());

        let handle = unsafe {
            LoadImageW(
                std::ptr::null_mut(),
                wide_path.as_ptr(),
                IMAGE_ICON,
                width as i32,
                height as i32,
                LR_DEFAULTSIZE | LR_LOADFROMFILE,
            )
        };
        if !handle.is_null() {
            Ok(WinIcon::from_handle(handle as HICON))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }

    fn from_resource_inner_name(name: PCWSTR, size: Option<(u32, u32)>) -> Result<Self, BadIcon> {
        // width / height of 0 along with LR_DEFAULTSIZE tells windows to load the default icon size
        let (width, height) = size.unwrap_or((0, 0));
        let handle = unsafe {
            LoadImageW(
                util::get_instance_handle(),
                name,
                IMAGE_ICON,
                width as i32,
                height as i32,
                LR_DEFAULTSIZE,
            )
        };
        if !handle.is_null() {
            Ok(WinIcon::from_handle(handle as HICON))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }

    pub(crate) fn from_resource(
        resource_id: u16,
        size: Option<(u32, u32)>,
    ) -> Result<Self, BadIcon> {
        Self::from_resource_inner_name(resource_id as PCWSTR, size)
    }

    pub(crate) fn from_resource_name(
        resource_name: &str,
        size: Option<(u32, u32)>,
    ) -> Result<Self, BadIcon> {
        let wide_name = util::encode_wide(resource_name);
        Self::from_resource_inner_name(wide_name.as_ptr(), size)
    }
}

impl Drop for RaiiIcon {
    fn drop(&mut self) {
        unsafe { DestroyIcon(self.handle) };
    }
}

impl fmt::Debug for WinIcon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        (*self.inner).fmt(formatter)
    }
}
