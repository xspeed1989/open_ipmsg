//! 全局热键：Windows/macOS/X11 用 tauri-plugin-global-shortcut，Wayland 走
//! org.freedesktop.portal.GlobalShortcuts（Tauri 插件在 Wayland 上无效，
//! 注册只会产生误导性的「成功」）。

#[derive(Debug, PartialEq, Eq)]
pub enum Backend {
    /// 平台插件（Windows / macOS / X11）
    Plugin,
    /// Wayland portal GlobalShortcuts
    ///
    /// 只作为接口语义保留：Wayland 分支现在进监听循环、不再返回（成功与否由
    /// bind_and_listen 自己打日志），因此没有构造点。
    #[allow(dead_code)]
    Portal,
    /// 未注册（热键为空或平台不支持）
    Disabled(&'static str),
}

/// 规范形 "Ctrl+Alt+A" → XDG shortcuts 触发器 "CTRL+ALT+a"
///
/// 只有 Linux 的 portal 分支会调它（Windows/macOS 走插件，那条路径已被 cfg 摘掉），
/// 补一条 allow 让 Task 12/13 的构建保持零 warning。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
    // Wayland 上 Tauri 插件注册不上（XDG 不允许客户端抢全局按键），走 portal。
    // portal 模块只在 Linux 编译，用 cfg 保证 Windows/macOS 也能编过。
    //
    // 注意：Wayland 分支**不返回** —— bind_and_listen 内部会进监听循环，直到进程
    // 结束；调用方必须已经在自己线程里（lib.rs 里就是 spawn 出去的）。
    #[cfg(target_os = "linux")]
    if crate::screenshot::is_wayland() {
        portal::bind_and_listen(app, combo);
        return Backend::Disabled("热键监听已结束");
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
    const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
    const REQ_IFACE: &str = "org.freedesktop.portal.Request";
    const ID: &str = "screenshot";

    /// Activated 信号体：(session_handle o, shortcut_id s, timestamp t, options a{sv})。
    /// 第一个参数必须是 OwnedObjectPath —— portal 发的是 (osta{sv})，写成 String 会
    /// SignatureMismatch，而监听循环里的 `else { continue }` 会把它静默吞掉（现象：
    /// 绑定成功、按键没反应）。监听线程与测试共用这一个定义，避免测试自己抄一份
    /// 导致类型退化时测试照样绿。
    pub(super) type ActivatedBody =
        (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>);

    /// Wayland 绑定 + 监听（**不返回**，直到进程结束）。
    ///
    /// 全程只用一条总线连接：portal 的 Activated 是「定向信号」，只发给创建 session
    /// 的那条连接（g_dbus_connection_emit_signal 的 destination 是 session->sender），
    /// 另开连接永远收不到；而且 session 属于创建它的连接 —— 连接一析构，portal 的
    /// close_sessions_for_sender 会把 session 连同 KDE 侧的快捷键绑定一起关掉。
    pub fn bind_and_listen(app: &tauri::AppHandle, combo: &str) {
        let trigger = match super::to_portal_trigger(combo) {
            Some(t) => t,
            None => {
                oim_log!("[shot] 非法快捷键：{combo}");
                return;
            }
        };
        match bind_inner(&trigger) {
            Ok(conn) => {
                oim_log!("[shot] 全局热键后端：Portal（{combo}）");
                listen_activated(&conn, app);
            }
            Err(e) => oim_log!("[shot] Wayland 热键绑定失败：{e}（可用 --screenshot 自行绑定快捷键）"),
        }
    }

    /// CreateSession → BindShortcuts，返回**建会话用的那条连接**：调用方一直持有它，
    /// session 与 KDE 侧的绑定才不会被回收（见 bind_and_listen 的注释）。
    fn bind_inner(trigger: &str) -> Result<Connection, String> {
        let conn = Connection::session().map_err(|e| format!("无法连接会话总线: {e}"))?;
        let gs = Proxy::new(&conn, DEST, PATH, IFACE).map_err(|e| e.to_string())?;

        // 1) 建会话（先订阅 Response 再调用）
        let token = format!("oimsess{}", std::process::id());
        let handle = request_path(&conn, &token)?;
        let req = Proxy::new(&conn, DEST, handle.as_str(), REQ_IFACE).map_err(|e| e.to_string())?;
        let mut signals = req.receive_signal("Response").map_err(|e| e.to_string())?;

        let mut opts: HashMap<&str, Value> = HashMap::new();
        opts.insert("handle_token", Value::from(token.as_str()));
        opts.insert("session_handle_token", Value::from(token.as_str()));
        let returned: OwnedObjectPath = gs
            .call("CreateSession", &(opts))
            .map_err(|e| format!("PORTAL_MISSING: {e}"))?;
        signals = retarget(&conn, signals, &returned, &handle)?;
        oim_log!("[shot] 等待 portal 建会话响应：{handle}");
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

        // 2) 绑定快捷键（KDE/GNOME 会弹一次系统确认框）
        let token2 = format!("oimbind{}", std::process::id());
        let handle2 = request_path(&conn, &token2)?;
        let req2 = Proxy::new(&conn, DEST, handle2.as_str(), REQ_IFACE).map_err(|e| e.to_string())?;
        let mut signals2 = req2.receive_signal("Response").map_err(|e| e.to_string())?;

        let mut sc_opts: HashMap<&str, Value> = HashMap::new();
        sc_opts.insert("description", Value::from("截图"));
        sc_opts.insert("preferred_trigger", Value::from(trigger));
        let shortcuts: Vec<(&str, HashMap<&str, Value>)> = vec![(ID, sc_opts)];
        let mut bind_opts: HashMap<&str, Value> = HashMap::new();
        bind_opts.insert("handle_token", Value::from(token2.as_str()));
        let returned2: OwnedObjectPath = gs
            .call("BindShortcuts", &(session_path, shortcuts, "", bind_opts))
            .map_err(|e| format!("绑定快捷键失败: {e}"))?;
        signals2 = retarget(&conn, signals2, &returned2, &handle2)?;
        oim_log!("[shot] 等待 portal 绑定响应（首次会弹系统确认框）：{handle2}");
        let (code2, res2) = next_response(&mut signals2)?;
        if code2 != 0 {
            return Err(format!("快捷键绑定被拒绝（响应码 {code2}）"));
        }
        oim_log!("[shot] Wayland 热键已绑定：{trigger}（响应 {res2:?}）");
        Ok(conn)
    }

    /// 在同一条连接上收 Activated（定向信号，见 bind_and_listen 的注释）。
    /// 循环直到进程结束 —— 这就是 register 的 Wayland 分支不返回的原因。
    fn listen_activated(conn: &Connection, app: &tauri::AppHandle) {
        let proxy = match Proxy::new(conn, DEST, PATH, IFACE) {
            Ok(p) => p,
            Err(e) => {
                oim_log!("[shot] 热键监听不可用（{e}）—— 可用 --screenshot 自行绑定快捷键");
                return;
            }
        };
        let mut sig = match proxy.receive_signal("Activated") {
            Ok(s) => s,
            Err(e) => {
                oim_log!("[shot] 热键监听不可用（{e}）—— 可用 --screenshot 自行绑定快捷键");
                return;
            }
        };
        for msg in &mut sig {
            let Ok((_session, id, _ts, _opts)): Result<ActivatedBody, _> = msg.body().deserialize()
            else {
                continue;
            };
            if id == ID {
                if let Err(e) = crate::screenshot::trigger(app) {
                    oim_log!("[shot] 热键触发失败：{}", e.message());
                }
            }
        }
    }

    fn request_path(conn: &Connection, token: &str) -> Result<String, String> {
        let sender = conn
            .unique_name()
            .map(|n| n.trim_start_matches(':').replace('.', "_"))
            .ok_or("会话总线没有唯一名")?;
        Ok(format!("/org/freedesktop/portal/desktop/request/{sender}/{token}"))
    }

    /// 返回的请求路径与预期不符时（handle_token 撞车时 portal 会补 /<random>），
    /// 把订阅改挂到实际路径上 —— 否则会在一个永远不会来 Response 的路径上死等。
    /// 与 screenshot.rs 里处理返回 handle 的写法一致。
    fn retarget<'m>(
        conn: &Connection,
        signals: zbus::blocking::proxy::SignalIterator<'m>,
        returned: &OwnedObjectPath,
        expected: &str,
    ) -> Result<zbus::blocking::proxy::SignalIterator<'m>, String> {
        if returned.as_str() == expected {
            return Ok(signals);
        }
        oim_log!(
            "[shot] portal 返回了另一个请求路径 {returned}（预期 {expected}），改挂订阅"
        );
        let req = Proxy::new(conn, DEST, returned.as_str(), REQ_IFACE).map_err(|e| e.to_string())?;
        req.receive_signal("Response").map_err(|e| e.to_string())
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

    /// Activated 的信号体是 (o session_handle, s shortcut_id, t timestamp, a{sv})：
    /// 第一个参数是**对象路径**而不是字符串。两个方向都钉住，防止监听类型退化：
    /// - 反向（先跑）：同一个报文**不能**按 (String, …) 解析 —— 一旦能，监听循环就会
    ///   把每条 Activated 都静默 continue（现象是「KDE 里绑好了、按 Alt+A 没反应」）；
    /// - 正向：能按监听线程实际使用的 portal::ActivatedBody 解析出来。
    /// 这里刻意用共享类型而不是自己再抄一份元组，否则监听侧退回 String 时测试照样绿。
    #[cfg(target_os = "linux")]
    #[test]
    fn activated_wire_shape_is_object_path_not_string() {
        use std::collections::HashMap;
        use zbus::zvariant::{ObjectPath, OwnedValue, Value};

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

        // 反向：session_handle 是对象路径，按字符串解析必须失败
        let as_string: Result<(String, String, u64, HashMap<String, OwnedValue>), _> =
            msg.body().deserialize();
        assert!(
            as_string.is_err(),
            "session_handle 是对象路径（o）：能按 String 解析说明监听类型又退化了"
        );

        // 正向：监听线程用的就是这个类型
        let (handle, id, ts, _opts): portal::ActivatedBody = msg
            .body()
            .deserialize()
            .expect("ActivatedBody 必须能解析 (osta{sv})");
        assert_eq!(handle.as_str(), session);
        assert_eq!(id, "screenshot");
        assert_eq!(ts, 1234);
    }
}
