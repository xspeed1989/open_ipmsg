//! 开机启动：把「当前可执行文件」登记到系统的登录自启位置。
//!
//! 自实现而非引入 tauri-plugin-autostart：三平台各自写的东西都很小，而这个
//! 仓库已经很在意依赖树（见 Cargo.toml 里 zbus / tray-icon 的长注释），
//! `dirs` 又本就是现有传递依赖，不需要再为 30 行平台代码多背一个 crate。
//!
//! - Windows：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下写
//!   `OpenIPMsg` 值（只动当前用户，不需要管理员权限）
//! - macOS：`~/Library/LaunchAgents/io.github.open-ipmsg.app.plist`（LaunchAgent
//!   在下次登录时由 launchd 读取；不需要当场 `launchctl bootstrap`）
//! - Linux：`$XDG_CONFIG_HOME/autostart/open-ipmsg.desktop`（freedesktop 自启规范，
//!   GNOME/KDE/XFCE 通吃）
//!
//! **写的是什么路径**：`std::env::current_exe()` 的实名化结果，再经
//! `normalized_for_entry()` 去掉 Windows 的拓展长度前缀 —— 复制到 `~/.local/bin`
//! 的是软链时，登记的是软链目标。写入形态同时就是「是否已经登记过」的判据
//! （`is_enabled()` 会把指向别处的、以及格式坏掉的旧条目都判为未登记，
//! 于是启动时能被 `apply_choice()` 重写成正确形态）。
//!
//! 平台分支只在对应平台上编译，所以文本生成与路径归一化都抽成纯函数并单测
//! （踩过的坑：verbatim 路径曾直接把 `//?/C:/...` 写进注册表，见 `strip_windows_verbatim`）。

use std::path::{Path, PathBuf};

/// 登录自启条目的名字（注册表值名 / plist 文件名与 Label / .desktop 文件名）
pub const ENTRY_NAME: &str = "OpenIPMsg";

/// Linux 自启项的桌面文件名
#[cfg(target_os = "linux")]
const LINUX_DESKTOP_FILE: &str = "open-ipmsg.desktop";

/// macOS LaunchAgent 文件名（用 bundle identifier，与 tauri.conf.json 一致）
#[cfg(target_os = "macos")]
const MACOS_PLIST_FILE: &str = "io.github.open-ipmsg.app.plist";

/// 注册表 Run 键（当前用户）
#[cfg(target_os = "windows")]
const WINDOWS_RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/* ================= 可执行文件路径 ================= */

/// 当前可执行文件的实名化路径。
///
/// `current_exe()` 在 Linux 上返回 `/proc/self/exe` 的内容（本身已是实名），
/// 但通过软链启动的其它平台会原样返回软链路径 —— 自启条目要指向真实文件，
/// 否则用户删掉软链就等于关掉了自启。`canonicalize` 失败时退回原路径。
pub fn current_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("E_AUTOSTART_FAILED|取当前可执行文件路径失败：{e}"))?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// 剥离 Windows 拓展长度前缀（纯字符串函数，**各平台都编译**）。
///
/// 拆出来是为了能在 Linux 上跑测试：真正的剥离逻辑如果藏进 `cfg!(windows)` 分支里，
/// 本机（Linux）跑测试只会走 identity 分支，等于**修复代码从没被执行过**。
/// 这个函数与平台无关 —— 传什么字符串就按什么字符串处理。
///
/// Rust 显示 verbatim 路径时用正斜杠（`//?/C:/...`），而 Win32 文档里的写法是
/// `\\?\C:\...`，两种前缀都认；`//./` 与 `\\.\`（设备路径）同理一并剥掉。
fn strip_windows_verbatim(s: &str) -> Option<String> {
    for prefix in ["//?/", r"\\?\", "//./", r"\\.\"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return Some(rest.replace('/', "\\"));
        }
    }
    None
}

/// 把路径规范成「外部程序也认」的形态：**去掉 Windows 拓展长度前缀**。
///
/// `std::fs::canonicalize` 在 Windows 上走 `GetFinalPathNameByHandleW`，返回 verbatim
/// （拓展长度）路径 —— 本该是 `\\?\C:\dir\app.exe`，而 Rust 的 `Path` 显示出来是
/// `//?/C:/dir/app.exe`（分隔符成了正斜杠）。这串东西 `std::fs` 自己认，写进注册表
/// Run 项却是坏的：Windows 外壳不认那串前缀、也不认 `//?/C:/...` 这种写法，开机就是
/// 「找不到文件」（本机实测踩到过）。所以**凡是写到应用之外**的路径都过这里。
///
/// `\\?\` 前缀本身的作用（绕过 MAX_PATH 限制、跳过路径解析）在自启场景里没有意义，
/// 去掉之后外壳才能按常规方式解析它。
fn normalized_for_entry(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        if let Some(rest) = strip_windows_verbatim(&s) {
            return rest;
        }
    }
    s.into_owned()
}

// Windows：注册表句柄的关闭函数在三个平台函数里都要用，提到模块级导入
// （其余绑定只在各自的函数里 use，避免未使用导入告警）
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Registry::RegCloseKey;

/* ================= 纯文本生成（可单测） ================= */

// 下面这一组只在 Linux/macOS 的条目文本里用得到（Windows 走注册表），
// 按同一条 cfg 归组，避免 Windows 目标下报一堆 dead_code
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod text {
    use std::path::Path;

    use super::ENTRY_NAME;

    /// XML 文本转义（& 必须第一个换）。只有 macOS 的 plist 是 XML
    #[cfg(target_os = "macos")]
    pub fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    /// 按**字符**（不是字节）截断，避免切断多字节 UTF-8（只有 macOS 的 CFBundleName 用）
    #[cfg(target_os = "macos")]
    pub fn truncate_chars(s: &str, max: usize) -> String {
        if s.chars().count() <= max {
            return s.to_string();
        }
        s.chars().take(max).collect()
    }

    /// freedesktop 桌面条目的 Exec 值：带空格/引号/反斜杠的路径必须按规范加引号转义
    /// （`\` 与 `"` 反斜杠转义、`` ` `` 与 `$` 用 `\` 转义；其余保持原样）。
    pub fn exec_value(exe: &str) -> String {
        let needs_quote = exe.is_empty()
            || exe
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '`' | '$'));
        if !needs_quote {
            return exe.to_string();
        }
        let mut out = String::with_capacity(exe.len() + 2);
        out.push('"');
        for c in exe.chars() {
            if matches!(c, '"' | '\\' | '`' | '$') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    }

    /// Linux 自启条目内容。
    ///
    /// `Path=` 是 systemd 的扩展键，在其它桌面环境里被忽略而不会报错，
    /// 顺手写上可以让「自启进程的工作目录」跟手动启动一致（相对路径的下载目录等）。
    /// `X-GNOME-Autostart-enabled` 是 GNOME 的显式开关（缺失时默认按启用处理）。
    #[cfg(target_os = "linux")]
    pub fn linux_desktop_entry(exe: &str) -> String {
        let path = Path::new(exe);
        let dir = path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let name = format!(
            "{}（局域网消息）",
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| ENTRY_NAME.into())
        );
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Version=1.0\n\
             Name={name}\n\
             Comment=IP Messenger 局域网即时通讯\n\
             Exec={exec}\n\
             Path={dir}\n\
             Terminal=false\n\
             NoDisplay=false\n\
             StartupNotify=false\n\
             X-GNOME-Autostart-enabled=true\n",
            name = name,
            exec = exec_value(exe),
            dir = dir,
        )
    }

    /// macOS LaunchAgent plist 内容。
    ///
    /// 这里手写最小 plist（dict/array/string/bool 四种类型），不为一次性的
    /// 四个键引入 plist 序列化依赖。
    #[cfg(target_os = "macos")]
    pub fn macos_plist(exe: &str) -> String {
        let label = super::MACOS_PLIST_FILE.trim_end_matches(".plist");
        let name = truncate_chars(ENTRY_NAME, 15);
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
	</array>
	<key>CFBundleName</key>
	<string>{name}</string>
	<key>RunAtLoad</key>
	<true/>
</dict>
</plist>
"#,
            label = xml_escape(label),
            exe = xml_escape(exe),
            name = xml_escape(&name),
        )
    }
}

#[cfg(target_os = "linux")]
use text::linux_desktop_entry;
#[cfg(target_os = "macos")]
use text::macos_plist;

/// Windows 注册表 Run 值的写法：路径带空格时**必须**带引号，否则系统按
/// 第一个空格切开当命令 + 参数，`C:\Program Files\...` 一开机就报找不到文件。
/// 只有注册表写入用得到它（文本条目自带引号转义规则），故按平台限定编译；
/// 引号规则本身跨平台可测，见 `windows_run_value_quotes_paths_with_spaces`。
#[cfg(target_os = "windows")]
fn windows_run_value(exe: &str) -> String {
    if exe.contains(' ') {
        format!("\"{exe}\"")
    } else {
        exe.to_string()
    }
}

/// 从命令行（或 Run 值）里取出可执行文件路径：反过来解析 `windows_run_value`
/// 的引号包裹，用于判断已登记的是不是当前这个 exe。
fn exe_from_command(cmd: &str) -> String {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        return match rest.find('"') {
            Some(end) => rest[..end].to_string(),
            // 只有开头一个引号：按未闭合处理，取余下全部，避免整条命令被当成路径
            None => rest.to_string(),
        };
    }
    cmd.split_whitespace().next().unwrap_or_default().to_string()
}

/// 已登记的条目是否**就是我们会写进去的那个值**。
///
/// 这里刻意做「与写入形态逐字比较」，而不是只比 canonical 后的路径：
/// 旧版本（或手工误配）可能登记成一个**格式坏了**的值 —— 典型是 Windows 上的
/// `//?/C:/dir/app.exe`（verbatim 前缀 + 正斜杠，开机根本起不来）。那种值
/// canonical 之后与我们自己的路径**相等**，只比路径就会判成「已登记」，
/// 于是启动对齐永远不会去修它，坏值永久留在系统里。
/// 逐字比较则把它判成「不匹配」→ 启动时按正确形态重写一遍。
///
/// 大小写只在 Windows 上折叠：`C:\App` 与 `c:\app` 是同一个文件，
/// 不折叠会让每次启动都白写一次注册表。
fn registered_matches(registered: &str, current: &Path) -> bool {
    let want = normalized_for_entry(current);
    let got = registered.trim();
    if cfg!(windows) {
        want.eq_ignore_ascii_case(got)
    } else {
        want == got
    }
}

/// 写入文本文件（父目录不存在则创建；已有文件保留原权限位，避免把用户
/// 手工改过的 0o600 覆盖成默认值）。只有 Linux/macOS 的条目是文本文件
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_text(path: &Path, text: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("E_AUTOSTART_FAILED|自启条目路径没有上级目录：{}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("E_AUTOSTART_FAILED|创建目录失败 {}：{e}", parent.display()))?;
    std::fs::write(path, text)
        .map_err(|e| format!("E_AUTOSTART_FAILED|写入自启条目失败 {}：{e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o644);
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    Ok(())
}

/* ================= 路径（平台） ================= */

/// Linux 自启条目路径：`$XDG_CONFIG_HOME/autostart`，未设置时按规范退回 `~/.config`
#[cfg(target_os = "linux")]
fn entry_path() -> Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => dirs::home_dir()
            .ok_or_else(|| "E_AUTOSTART_FAILED|找不到用户主目录".to_string())?
            .join(".config"),
    };
    Ok(base.join("autostart").join(LINUX_DESKTOP_FILE))
}

/// macOS LaunchAgent 路径：`~/Library/LaunchAgents/<bundle id>.plist`
#[cfg(target_os = "macos")]
fn entry_path() -> Result<PathBuf, String> {
    Ok(dirs::home_dir()
        .ok_or_else(|| "E_AUTOSTART_FAILED|找不到用户主目录".to_string())?
        .join("Library")
        .join("LaunchAgents")
        .join(MACOS_PLIST_FILE))
}

/* ================= 对外接口 ================= */

/// 登记开机启动（覆盖写：路径变化时不会留下指向旧 exe 的条目）
pub fn enable(exe: &Path) -> Result<(), String> {
    if !exe.is_file() {
        return Err(format!(
            "E_AUTOSTART_FAILED|可执行文件不存在：{}（开发构建的临时路径、已被移动或删除的程序文件都登记不了，请用安装版）",
            exe.display()
        ));
    }
    enable_platform(exe)
}

/// 取消开机启动。条目本来就不存在时视为成功（幂等）
pub fn disable() -> Result<(), String> {
    disable_platform()
}

/// 系统里是否已按**正确形态**登记当前这个 exe 为开机启动。
/// 格式坏掉（如 Windows 的 `//?/C:/...`）或指向别处都算「未登记」，
/// 于是 `apply_choice` 会在启动时把它重写成正确形态。
pub fn is_enabled(exe: &Path) -> bool {
    registered_exe().map(|r| registered_matches(&r, exe)).unwrap_or(false)
}

/// 启动时按用户的选择对齐系统条目，返回是否真的动过。
///
/// - `want = true`：系统里没有条目、或条目指向别处（换过安装位置、被清理工具删掉）
///   时按当前 exe 补写一次
/// - `want = false`：条目还指着当前 exe（上一次「登记成功但配置没落盘」的残留）
///   时删掉；指向别处的条目不动 —— 那是用户自己配的东西
///
/// 失败只上报给调用方记日志 —— 自启没登记上不该挡住启动。
///
/// **debug 构建（`tauri dev`）只做减法**：那时的 exe 在 target/debug 下，路径是
/// 临时的，登录时既可能已被清掉、也会白白拉起第二个实例，所以不自动补登记；
/// 但「配置说关」的清理照做（用户显式点开关时也仍会登记，方便本机验证）。
pub fn apply_choice(want: bool, exe: &Path) -> Result<bool, String> {
    if want {
        if cfg!(debug_assertions) || is_enabled(exe) {
            return Ok(false);
        }
        enable(exe)?;
        return Ok(true);
    }
    if is_enabled(exe) {
        disable()?;
        return Ok(true);
    }
    Ok(false)
}

/* ================= 平台实现 ================= */

#[cfg(target_os = "linux")]
fn enable_platform(exe: &Path) -> Result<(), String> {
    write_text(&entry_path()?, &linux_desktop_entry(&normalized_for_entry(exe)))
}

#[cfg(target_os = "linux")]
fn disable_platform() -> Result<(), String> {
    let path = entry_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("E_AUTOSTART_FAILED|删除自启条目失败 {}：{e}", path.display())),
    }
}

/// 读回 .desktop 里的 Exec 值（只认 `[Desktop Entry]` 段里的那一行）
#[cfg(target_os = "linux")]
fn registered_exe() -> Result<String, String> {
    let path = entry_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(format!("E_AUTOSTART_FAILED|读取自启条目失败 {}：{e}", path.display())),
    };
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = line
            .strip_prefix("Exec=")
            .or_else(|| line.strip_prefix("Exec ="))
        {
            return Ok(exe_from_command(v));
        }
    }
    Ok(String::new())
}

#[cfg(target_os = "macos")]
fn enable_platform(exe: &Path) -> Result<(), String> {
    // plist 是 XML：路径里的 & < > 必须转义（见 macos_plist）
    write_text(&entry_path()?, &macos_plist(&normalized_for_entry(exe)))
}

#[cfg(target_os = "macos")]
fn disable_platform() -> Result<(), String> {
    let path = entry_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("E_AUTOSTART_FAILED|删除自启条目失败 {}：{e}", path.display())),
    }
}

/// 读回 plist 的 `ProgramArguments` 第一项（我们只写一个参数，用不上通用解析器）
#[cfg(target_os = "macos")]
fn registered_exe() -> Result<String, String> {
    let path = entry_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(format!("E_AUTOSTART_FAILED|读取自启条目失败 {}：{e}", path.display())),
    };
    let after = match text.find("<key>ProgramArguments</key>") {
        Some(i) => &text[i..],
        None => return Ok(String::new()),
    };
    match (after.find("<string>"), after.find("</string>")) {
        (Some(s), Some(e)) if e > s + 8 => {
            let raw = &after[s + 8..e];
            Ok(raw
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&"))
        }
        _ => Ok(String::new()),
    }
}

#[cfg(target_os = "windows")]
#[allow(non_snake_case)] // 与 Win32 原名一致，便于对照文档
fn enable_platform(exe: &Path) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    let subkey = wide(WINDOWS_RUN_KEY);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(format!(
            "E_AUTOSTART_FAILED|打开注册表 Run 键失败（错误码 {rc}）"
        ));
    }
    let name = wide(ENTRY_NAME);
    // REG_SZ 的字节长度**必须**含结尾 NUL
    let value = wide(&windows_run_value(&normalized_for_entry(exe)));
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2)
    };
    let rc = unsafe {
        RegSetValueExW(
            hkey,
            name.as_ptr(),
            0,
            REG_SZ,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };
    unsafe { RegCloseKey(hkey) };
    if rc != 0 {
        return Err(format!("E_AUTOSTART_FAILED|写入注册表失败（错误码 {rc}）"));
    }
    Ok(())
}

// `RegCreateKeyExW` 的就地声明。
//
// windows-sys 0.61 把这一条签名里的 `lpSecurityAttributes` 引到
// `Win32::Security::SECURITY_ATTRIBUTES`，于是用它就必须同时打开
// `Win32_System_Security` 特性 —— 而本仓库锁定的 0.61.2 在 crates.io 索引里
// 并没有那个特性（0.61.3 才补上），为一个恒为 NULL 的参数去动整个
// windows-sys 的版本与特性面不划算。其余注册表函数不需要 Security，
// 仍走 windows-sys（见上面的 use）。
#[cfg(target_os = "windows")]
#[link(name = "advapi32")]
extern "system" {
    #[allow(non_snake_case)]
    fn RegCreateKeyExW(
        hkey: windows_sys::Win32::System::Registry::HKEY,
        lpsubkey: *const u16,
        reserved: u32,
        lpclass: *const u16,
        dwoptions: u32,
        samdesired: u32,
        lpsecurityattributes: *const core::ffi::c_void,
        phkresult: *mut windows_sys::Win32::System::Registry::HKEY,
        lpdwdisposition: *mut u32,
    ) -> u32;
}

#[cfg(target_os = "windows")]
fn disable_platform() -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{
        RegDeleteValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
    };

    let subkey = wide(WINDOWS_RUN_KEY);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_WRITE, &mut hkey) };
    if rc != 0 {
        // 键不存在 = 从没登记过，视为已关闭
        return Ok(());
    }
    let name = wide(ENTRY_NAME);
    let rc = unsafe { RegDeleteValueW(hkey, name.as_ptr()) };
    unsafe { RegCloseKey(hkey) };
    // ERROR_FILE_NOT_FOUND(2)：值本来就不存在 —— 幂等
    if rc != 0 && rc != 2 {
        return Err(format!("E_AUTOSTART_FAILED|删除注册表值失败（错误码 {rc}）"));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn registered_exe() -> Result<String, String> {
    use windows_sys::Win32::System::Registry::{
        RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_SZ,
    };

    let subkey = wide(WINDOWS_RUN_KEY);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) };
    if rc != 0 {
        return Ok(String::new());
    }
    let name = wide(ENTRY_NAME);
    let mut ty: u32 = 0;
    let mut len: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null(),
            &mut ty,
            std::ptr::null_mut(),
            &mut len,
        )
    };
    if rc != 0 {
        unsafe { RegCloseKey(hkey) };
        return Ok(String::new());
    }
    // 长度上限护栏（MAX_PATH 语义）：异常大的值不读，当作未登记
    if ty != REG_SZ || len == 0 || len > 32 * 1024 {
        unsafe { RegCloseKey(hkey) };
        return Ok(String::new());
    }
    let mut buf = vec![0u8; len as usize];
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null(),
            &mut ty,
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    unsafe { RegCloseKey(hkey) };
    if rc != 0 {
        return Ok(String::new());
    }
    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    Ok(exe_from_command(&String::from_utf16_lossy(&units[..end])))
}

#[cfg(target_os = "windows")]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 未覆盖的平台（FreeBSD 等）：明确报「不支持」，不做静默失败
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn enable_platform(_exe: &Path) -> Result<(), String> {
    Err("E_AUTOSTART_FAILED|当前平台不支持开机启动设置".into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn disable_platform() -> Result<(), String> {
    Err("E_AUTOSTART_FAILED|当前平台不支持开机启动设置".into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn registered_exe() -> Result<String, String> {
    Ok(String::new())
}

/* ================= 测试 ================= */

#[cfg(test)]
mod tests {
    use super::*;

    /// 下面两个 Linux 用例都要改 `XDG_CONFIG_HOME`（进程级环境变量），
    /// 而 `cargo test` 默认多线程跑同一个进程：不加锁就会互相把对方的
    /// 自启目录指到自己的临时目录，偶发红。
    #[cfg(target_os = "linux")]
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn exec_value_quotes_only_when_needed() {
        assert_eq!(text::exec_value("/usr/bin/open-ipmsg"), "/usr/bin/open-ipmsg");
        assert_eq!(text::exec_value("/opt/a b/open-ipmsg"), "\"/opt/a b/open-ipmsg\"");
        // `"` `\` `` ` `` `$` 在桌面条目规范里必须反斜杠转义
        assert_eq!(text::exec_value("/opt/a\"b/app"), "\"/opt/a\\\"b/app\"");
        assert_eq!(text::exec_value("/opt/a\\b/app"), "\"/opt/a\\\\b/app\"");
        assert_eq!(text::exec_value("/opt/$HOME/app"), "\"/opt/\\$HOME/app\"");
    }

    // Linux/macOS 专有的文本生成用例归在一组：Windows 目标下 text 模块不参与编译，
    // 这组用例也就整体不编译（Windows 自己的用例在下面，与平台无关的解析用例在其后）
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod entry_text {
        use super::*;

        #[test]
        #[cfg(target_os = "linux")]
        fn linux_entry_keeps_multibyte_paths_intact() {
            let exe = "/home/allen/下载/open-ipmsg";
            let text = linux_desktop_entry(exe);
            assert!(text.starts_with("[Desktop Entry]"));
            assert!(text.contains("Type=Application"));
            assert!(text.contains("X-GNOME-Autostart-enabled=true"));
            // 非 ASCII 不需要引号（规范只要求空白与保留字符加引号）：路径必须原样保留
            assert!(
                text.contains("Exec=/home/allen/下载/open-ipmsg"),
                "带非 ASCII 的路径必须原样保留：\n{text}"
            );
            // Path= 取所在目录，供自启进程的工作目录
            assert!(text.contains("Path=/home/allen/下载"));

            // 中文目录里再带空格（真实场景：~/下载 被改成 ~/我的 下载）：这时才要引号
            let spaced = linux_desktop_entry("/home/allen/我的 下载/open-ipmsg");
            assert!(
                spaced.contains("Exec=\"/home/allen/我的 下载/open-ipmsg\""),
                "含空格的路径必须整体加引号：\n{spaced}"
            );
            assert_eq!(
                exe_from_command(
                    spaced.lines().find(|l| l.starts_with("Exec=")).unwrap()["Exec=".len()..].trim()
                ),
                "/home/allen/我的 下载/open-ipmsg"
            );
        }

        #[test]
        #[cfg(target_os = "linux")]
        fn linux_entry_round_trips_through_exec_parse() {
            for exe in [
                "/usr/bin/open-ipmsg",
                "/opt/My App/open-ipmsg",
                "/home/allen/下载/open-ipmsg",
            ] {
                let text = linux_desktop_entry(exe);
                let line = text
                    .lines()
                    .find(|l| l.starts_with("Exec="))
                    .expect("自启条目必须有 Exec 行");
                assert_eq!(exe_from_command(&line["Exec=".len()..]), exe, "Exec 解析往返");
            }
        }

        #[test]
        #[cfg(target_os = "macos")]
        fn macos_plist_escapes_xml_and_loads_at_login() {
            let text = macos_plist("/Applications/A & B/open-ipmsg");
            assert!(text.contains("<key>RunAtLoad</key>"));
            assert!(text.contains("<true/>"));
            assert!(
                text.contains("<string>/Applications/A &amp; B/open-ipmsg</string>"),
                "XML 特殊字符必须转义，否则 launchd 读不了这份 plist：\n{text}"
            );
            assert!(text.contains("<key>ProgramArguments</key>"));
            assert!(!text.contains("& "), "裸 & 会破坏 plist 的 XML 结构");
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_run_value_quotes_paths_with_spaces() {
        assert_eq!(
            windows_run_value(r"C:\Program Files\OpenIPMsg\open-ipmsg.exe"),
            "\"C:\\Program Files\\OpenIPMsg\\open-ipmsg.exe\""
        );
        assert_eq!(
            windows_run_value(r"C:\open-ipmsg.exe"),
            r"C:\open-ipmsg.exe"
        );
        // 往返：带引号的值解析回原路径（不含引号）
        let v = windows_run_value(r"C:\Program Files\x\open-ipmsg.exe");
        assert_eq!(exe_from_command(&v), r"C:\Program Files\x\open-ipmsg.exe");
    }

    #[test]
    fn exe_from_command_handles_unquoted_and_broken_values() {
        assert_eq!(exe_from_command("C:\\app.exe --tray"), "C:\\app.exe");
        assert_eq!(exe_from_command("  /usr/bin/open-ipmsg  "), "/usr/bin/open-ipmsg");
        // 只有开头引号（人工改坏的值）：不能把整条命令当成路径
        assert_eq!(exe_from_command("\"/opt/a b/open-ipmsg"), "/opt/a b/open-ipmsg");
        assert_eq!(exe_from_command(""), "");
    }

    /// Windows 上 `canonicalize` 返回的是 verbatim 路径，直接写进注册表 Run 项就是坏的
    /// （本机实测：注册表里落地成 `//?/C:/Users/.../open-ipmsg.exe`，开机起不来）。
    ///
    /// 剥离逻辑是纯函数，在 Linux 上也被真实执行 —— 不依赖 Windows 才能验证。
    #[test]
    fn strips_windows_verbatim_prefix() {
        // Rust 显示 verbatim 路径时用的是正斜杠（`//?/C:/...`）
        assert_eq!(
            strip_windows_verbatim("//?/C:/Users/Admin/Desktop/open-ipmsg.exe").as_deref(),
            Some(r"C:\Users\Admin\Desktop\open-ipmsg.exe"),
        );
        // Win32 文档里的反斜杠写法同样要认
        assert_eq!(
            strip_windows_verbatim(r"\\?\C:\dir\app.exe").as_deref(),
            Some(r"C:\dir\app.exe"),
        );
        // 设备路径前缀（\\.\ / //./）一并剥掉
        assert_eq!(
            strip_windows_verbatim(r"\\.\C:\dir\app.exe").as_deref(),
            Some(r"C:\dir\app.exe"),
        );
        assert_eq!(strip_windows_verbatim("//./PhysicalDrive0").as_deref(), Some("PhysicalDrive0"));
        // 普通路径不动（尤其别把 POSIX 路径的斜杠改掉）
        assert_eq!(strip_windows_verbatim("/usr/bin/open-ipmsg"), None);
        assert_eq!(strip_windows_verbatim(r"C:\dir\app.exe"), None);
        assert_eq!(strip_windows_verbatim("/home/allen/下载/open-ipmsg"), None);
        // 路径中间出现 `?` 不是前缀，不能动
        assert_eq!(strip_windows_verbatim(r"C:\a?b\app.exe"), None);
    }

    /// 判据用的是「与写入形态逐字比较」：格式坏了的值必须被判成未登记，
    /// 否则启动对齐永远不会去修它（Windows 上的 `//?/C:/...` 就是这么留下的）。
    #[test]
    fn registered_matches_is_strict_about_format() {
        let exe = if cfg!(windows) {
            PathBuf::from(r"C:\Users\Admin\Desktop\open-ipmsg.exe")
        } else {
            PathBuf::from("/home/allen/下载/open-ipmsg")
        };
        let want = normalized_for_entry(&exe);
        assert!(registered_matches(&want, &exe), "同一个值应当判为已登记");
        assert!(registered_matches(&format!("  {want}  "), &exe), "两端的空白要忽略");
        assert!(!registered_matches("", &exe), "空值等于没登记");
        assert!(!registered_matches("/usr/bin/other-app", &exe), "指向别的 exe 不算已登记");

        if cfg!(windows) {
            // 就是这个坏值害得开机起不来：必须判为「未登记」→ 启动时重写
            assert!(
                !registered_matches("//?/C:/Users/Admin/Desktop/open-ipmsg.exe", &exe),
                "verbatim 前缀的坏值必须被判成未登记，否则启动对齐不会修它"
            );
            // Windows 路径大小写不敏感：折叠后应判为同一个
            assert!(registered_matches(r"c:\users\admin\desktop\open-ipmsg.exe", &exe));
        }
    }

    /// 归一化入口在各平台的语义：Windows 剥前缀，其它平台原样返回
    #[test]
    fn normalization_is_windows_only_and_idempotent() {
        let verbatim = "//?/C:/Users/Admin/Desktop/open-ipmsg.exe";
        let expected = if cfg!(windows) {
            r"C:\Users\Admin\Desktop\open-ipmsg.exe"
        } else {
            verbatim
        };
        assert_eq!(normalized_for_entry(Path::new(verbatim)), expected);

        let plain = if cfg!(windows) { r"C:\dir\app.exe" } else { "/usr/bin/open-ipmsg" };
        assert_eq!(normalized_for_entry(Path::new(plain)), plain);
        // 幂等：已规范过的再走一遍不会变
        assert_eq!(
            normalized_for_entry(Path::new(plain)),
            normalized_for_entry(Path::new(&normalized_for_entry(Path::new(plain))))
        );
    }

    /// 真机（Windows）上的端到端检查：把**当前 exe**（canonicalize 出来的 verbatim
    /// 路径）写进注册表再读回来，落地的值必须既不带 `?` 前缀、也不用正斜杠。
    /// 在 Linux/macOS 上直接跳过：前缀是 Windows 专有现象，那两个平台证不了什么。
    #[test]
    fn windows_target_strips_verbatim_prefix_when_writing() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            return;
        }
        let exe = current_exe().expect("取当前 exe");
        let expect = normalized_for_entry(&exe);
        assert!(!expect.contains('?'), "写进自启条目的路径不该带拓展长度前缀：{expect}");
        // 真的写系统条目再读回来（会短暂改动当前用户的 Run 项，读完立刻删）
        enable(&exe).expect("登记自启");
        let registered = registered_exe().expect("读回注册表值");
        disable().expect("清理注册表项");
        assert_eq!(registered, expect, "注册表落地的路径必须是规范化后的形态");
        assert!(!registered.contains('?'), "注册表值里不该出现 ? 前缀：{registered}");
        assert!(!registered.contains('/'), "注册表值应该用反斜杠：{registered}");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn truncate_chars_never_splits_utf8() {
        assert_eq!(text::truncate_chars("OpenIPMsg", 15), "OpenIPMsg");
        assert_eq!(text::truncate_chars("局域网即时通讯客户端名字很长", 4), "局域网即");
    }

    /// Linux 平台分支的读写：临时目录里真的写一份 .desktop 再读回来。
    /// （`entry_path()` 走 `XDG_CONFIG_HOME`，测试里指到临时目录，不碰用户真实配置）
    #[test]
    #[cfg(target_os = "linux")]
    fn linux_enable_is_enabled_and_disable_round_trip() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("oim-autostart-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let exe_dir = tmp.join("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("open-ipmsg");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();

        assert!(!is_enabled(&exe), "还没登记时不应报告已启用");
        enable(&exe).expect("登记自启应当成功");
        assert!(is_enabled(&exe), "登记后应当报告已启用");
        let entry = tmp.join("autostart").join(LINUX_DESKTOP_FILE);
        assert!(entry.is_file(), "自启条目应落在 $XDG_CONFIG_HOME/autostart 下");
        let text = std::fs::read_to_string(&entry).unwrap();
        assert!(text.contains("open-ipmsg"), "条目里应带着可执行文件路径");

        // 指向别处的旧条目：判为「未登记当前 exe」（启动时会被 reconcile 重写）
        let other = exe_dir.join("other-app");
        std::fs::write(&other, b"#!/bin/sh\n").unwrap();
        assert!(!is_enabled(&other), "条目指向别的 exe 时不应算作当前应用已启用");

        disable().expect("取消自启应当成功");
        assert!(!entry.exists(), "取消后条目文件应被删除");
        assert!(!is_enabled(&exe));
        // 幂等：重复取消不报错
        disable().expect("重复取消也应成功");

        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 可执行文件不存在（开发构建被删、AppImage 被移走）时给出明确错误，
    /// 而不是写一条开机必然失败的条目
    #[test]
    fn enable_rejects_missing_executable() {
        #[cfg(target_os = "linux")]
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let missing = std::env::temp_dir().join("oim-not-here/definitely-missing-binary");
        let err = enable(&missing).expect_err("路径不存在时应当报错");
        assert!(err.starts_with("E_AUTOSTART_FAILED|"), "错误码前缀供前端 i18n 分流：{err}");
    }

    /// 启动对齐的两个方向：debug 构建不自动「加」，但「减」照做。
    #[test]
    #[cfg(target_os = "linux")]
    fn apply_choice_adds_only_in_release_and_removes_always() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("oim-reconcile-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("XDG_CONFIG_HOME", &tmp);
        let exe_dir = tmp.join("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("open-ipmsg");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();

        // 配置说关、条目不存在：什么都不做
        assert!(!apply_choice(false, &exe).unwrap());
        assert!(!is_enabled(&exe));

        // 配置说开：debug 构建不写（`cargo test` 走的正是这条），release 才补登记
        let added = apply_choice(true, &exe).unwrap();
        assert_eq!(
            added,
            !cfg!(debug_assertions),
            "debug 构建不该自动补登记（tauri dev 的 target/debug 路径是临时的）"
        );
        assert_eq!(is_enabled(&exe), !cfg!(debug_assertions));

        // 配置说关、条目存在：两个构建都做清理
        if !cfg!(debug_assertions) {
            assert!(apply_choice(false, &exe).unwrap(), "残留条目应当被清掉");
            assert!(!is_enabled(&exe));
            assert!(!apply_choice(false, &exe).unwrap(), "已经没有条目了，无事可做");
        }

        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
