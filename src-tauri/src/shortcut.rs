//! 全局热键：Windows/macOS/X11 用 tauri-plugin-global-shortcut，Wayland 走
//! org.freedesktop.portal.GlobalShortcuts（Tauri 插件在 Wayland 上无效，
//! 注册只会产生误导性的「成功」）。

#[derive(Debug, PartialEq, Eq)]
pub enum Backend {
    /// 平台插件（Windows / macOS / X11）
    Plugin,
    /// Wayland portal GlobalShortcuts
    Portal,
    /// 未注册（热键为空或平台不支持）
    Disabled(&'static str),
}

/// 规范形 "Ctrl+Alt+A" → XDG shortcuts 触发器 "CTRL+ALT+a"
pub fn to_portal_trigger(combo: &str) -> Option<String> {
    let parts: Vec<&str> = combo.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let (key, mods) = parts.split_last()?;
    let mut out: Vec<String> = mods
        .iter()
        .map(|m| match *m {
            "CmdOrCtrl" | "Super" | "Meta" => "LOGO".to_string(),
            other => other.to_uppercase(),
        })
        .collect();
    out.sort();
    let key_name = match *key {
        "Enter" => "Return",
        "Space" => "space",
        "Backspace" => "BackSpace",
        "PageUp" => "Page_Up",
        "PageDown" => "Page_Down",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "PrintScreen" => "Print",
        k if k.len() == 1 => return Some(format!("{}+{}", out.join("+"), k.to_lowercase())),
        k => k,
    };
    Some(format!("{}+{}", out.join("+"), key_name))
}

/// 注册全局热键；返回实际生效的后端
pub fn register(app: &tauri::AppHandle, combo: &str) -> Backend {
    if combo.trim().is_empty() {
        return Backend::Disabled("未配置热键");
    }
    // Wayland 上 Tauri 插件注册不上（XDG 不允许客户端抢全局按键），走 portal；
    // portal 模块只在 Linux 编译，用 cfg 保证 Windows/macOS 也能编过
    #[cfg(target_os = "linux")]
    if crate::screenshot::is_wayland() {
        return match portal::bind(app, combo) {
            Ok(()) => Backend::Portal,
            Err(e) => {
                oim_log!("[shot] Wayland 热键绑定失败：{e}（可用 --screenshot 自行绑定）");
                Backend::Disabled("当前桌面环境不支持全局热键")
            }
        };
    }
    match plugin::register(app, combo) {
        Ok(()) => Backend::Plugin,
        Err(e) => {
            oim_log!("[shot] 全局热键注册失败：{e}");
            Backend::Disabled("热键注册失败（可能被其他程序占用）")
        }
    }
}

/* ---------------- Windows / macOS / X11 ---------------- */

// 三平台共用同一份注册代码：Linux 上插件只在 X11 生效，Wayland 永远走下面的
// portal 分支（见 register 里的 is_wayland 判断），所以这里不需要平台分支。
mod plugin {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    pub fn register(app: &tauri::AppHandle, combo: &str) -> Result<(), String> {
        let shortcut: tauri_plugin_global_shortcut::Shortcut =
            combo.parse().map_err(|e| format!("非法快捷键 {combo}: {e}"))?;
        let handle = app.clone();
        app.global_shortcut()
            .on_shortcut(shortcut, move |_app, _sc, event| {
                if event.state() == ShortcutState::Pressed {
                    if let Err(e) = crate::screenshot::trigger(&handle) {
                        oim_log!("[shot] 热键触发失败：{}", e.message());
                    }
                }
            })
            .map_err(|e| e.to_string())
    }
}

/* ---------------- Wayland: portal GlobalShortcuts ---------------- */

#[cfg(target_os = "linux")]
mod portal {
    use std::collections::HashMap;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const ID: &str = "screenshot";

    /// CreateSession → BindShortcuts → 后台线程监听 Activated
    pub fn bind(app: &tauri::AppHandle, combo: &str) -> Result<(), String> {
        let trigger = super::to_portal_trigger(combo).ok_or("非法快捷键")?;
        let conn = Connection::session().map_err(|e| format!("无法连接会话总线: {e}"))?;

        // 1) 建会话（先订阅 Response 再调用）
        let token = format!("oimsess{}", std::process::id());
        let handle = request_path(&conn, &token)?;
        let req = Proxy::new(&conn, DEST, handle.as_str(), "org.freedesktop.portal.Request")
            .map_err(|e| e.to_string())?;
        let mut signals = req.receive_signal("Response").map_err(|e| e.to_string())?;

        let mut opts: HashMap<&str, Value> = HashMap::new();
        opts.insert("handle_token", Value::from(token.as_str()));
        opts.insert("session_handle_token", Value::from(token.as_str()));
        let gs = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.GlobalShortcuts")
            .map_err(|e| e.to_string())?;
        let _: OwnedObjectPath = gs
            .call("CreateSession", &(opts))
            .map_err(|e| format!("PORTAL_MISSING: {e}"))?;
        let (code, results) = next_response(&mut signals)?;
        if code != 0 {
            return Err(format!("创建快捷键会话被拒绝（响应码 {code}）"));
        }
        // portal 的 session_handle 是「被错误实现为 s 的对象路径」（规范原话）：
        // 响应里读出来是字符串（zvariant 只能借出 &str），但 BindShortcuts 的入参
        // 声明是 o，直接传字符串会被前端的类型检查拒掉 —— 实测报
        // InvalidArgs: "(sa(sa{sv})sa{sv})" 与预期的 "(oa(sa{sv})sa{sv})" 不匹配
        let session: &str = results
            .get("session_handle")
            .ok_or("响应里没有 session_handle")?
            .try_into()
            .map_err(|_| "session_handle 不是字符串")?;
        let session_path = ObjectPath::try_from(session)
            .map_err(|e| format!("session_handle 不是合法对象路径: {e}"))?;

        // 2) 绑定快捷键（KDE/GNOME 会弹一次确认框）
        let token2 = format!("oimbind{}", std::process::id());
        let handle2 = request_path(&conn, &token2)?;
        let req2 = Proxy::new(&conn, DEST, handle2.as_str(), "org.freedesktop.portal.Request")
            .map_err(|e| e.to_string())?;
        let mut signals2 = req2.receive_signal("Response").map_err(|e| e.to_string())?;

        let mut sc_opts: HashMap<&str, Value> = HashMap::new();
        sc_opts.insert("description", Value::from("截图"));
        sc_opts.insert("preferred_trigger", Value::from(trigger.as_str()));
        let shortcuts: Vec<(&str, HashMap<&str, Value>)> = vec![(ID, sc_opts)];
        let mut bind_opts: HashMap<&str, Value> = HashMap::new();
        bind_opts.insert("handle_token", Value::from(token2.as_str()));
        let _: OwnedObjectPath = gs
            .call("BindShortcuts", &(session_path, shortcuts, "", bind_opts))
            .map_err(|e| format!("绑定快捷键失败: {e}"))?;
        let (code2, res2) = next_response(&mut signals2)?;
        if code2 != 0 {
            return Err(format!("快捷键绑定被拒绝（响应码 {code2}）"));
        }
        oim_log!("[shot] Wayland 热键已绑定：{combo} → {trigger}（响应 {res2:?}）");

        // 3) 监听 Activated：收到就触发截图，直到进程退出
        let app2 = app.clone();
        std::thread::spawn(move || {
            let Ok(conn) = Connection::session() else { return };
            let Ok(proxy) = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.GlobalShortcuts")
            else {
                return;
            };
            let Ok(mut sig) = proxy.receive_signal("Activated") else { return };
            for msg in &mut sig {
                // Activated 的信号体是 (o session_handle, s shortcut_id, t timestamp, a{sv})：
                // 第一个参数是**对象路径**。写成 String 会 SignatureMismatch，而错误被
                // 下面的 continue 吞掉 —— 表现就是「快捷方式绑好了但按了没反应」，
                // 所以这里按 o 解析（见 tests::activated_signal_body_has_object_path_handle）。
                let Ok((_session, id, _ts, _opts)): Result<
                    (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>),
                    _,
                > = msg.body().deserialize()
                else {
                    continue;
                };
                if id == ID {
                    if let Err(e) = crate::screenshot::trigger(&app2) {
                        oim_log!("[shot] 热键触发失败：{}", e.message());
                    }
                }
            }
        });
        Ok(())
    }

    fn request_path(conn: &Connection, token: &str) -> Result<String, String> {
        let sender = conn
            .unique_name()
            .map(|n| n.trim_start_matches(':').replace('.', "_"))
            .ok_or("会话总线没有唯一名")?;
        Ok(format!("/org/freedesktop/portal/desktop/request/{sender}/{token}"))
    }

    fn next_response(
        signals: &mut zbus::blocking::proxy::SignalIterator<'_>,
    ) -> Result<(u32, HashMap<String, OwnedValue>), String> {
        let msg = signals.next().ok_or("portal 未返回响应")?;
        msg.body().deserialize().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_trigger_follows_xdg_shortcuts_spec() {
        assert_eq!(to_portal_trigger("Alt+A").as_deref(), Some("ALT+a"));
        assert_eq!(to_portal_trigger("Ctrl+Shift+S").as_deref(), Some("CTRL+SHIFT+s"));
        // Super/Windows → LOGO，多修饰键按字母序
        assert_eq!(to_portal_trigger("CmdOrCtrl+Alt+A").as_deref(), Some("ALT+LOGO+a"));
        // 主键用 xkbcommon 键名
        assert_eq!(to_portal_trigger("Ctrl+Enter").as_deref(), Some("CTRL+Return"));
        // 非法组合被拒
        assert_eq!(to_portal_trigger("A"), None);
        assert_eq!(to_portal_trigger(""), None);
    }

    /// portal 的 Activated 信号体是 (o session_handle, s shortcut_id, t timestamp, a{sv})：
    /// 第一个参数是**对象路径**而不是字符串。若按 String 反序列化会直接
    /// SignatureMismatch（实测报 `(ssta{sv})` 与 `(osta{sv})` 不符），监听线程会静默
    /// continue —— 热键永远不触发，且现场极难定位，所以把信号形状钉在测试里。
    #[test]
    fn activated_signal_body_has_object_path_handle() {
        use std::collections::HashMap;
        use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

        let session = "/org/freedesktop/portal/desktop/session/1_2/oimsess1";
        let msg = zbus::message::Message::signal(
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.GlobalShortcuts",
            "Activated",
        )
        .unwrap()
        .build(&(
            ObjectPath::try_from(session).unwrap(),
            "screenshot",
            1234u64,
            HashMap::<String, Value>::new(),
        ))
        .unwrap();

        // 目标类型与监听线程里的完全一致
        let (handle, id, ts, _opts): (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>) =
            msg.body().deserialize().unwrap();
        assert_eq!(handle.as_str(), session);
        assert_eq!(id, "screenshot");
        assert_eq!(ts, 1234);
    }
}
