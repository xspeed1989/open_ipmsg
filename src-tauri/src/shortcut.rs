//! 全局热键：Windows/macOS/X11 用 tauri-plugin-global-shortcut，Wayland 走
//! org.freedesktop.portal.GlobalShortcuts（Tauri 插件在 Wayland 上无效，
//! 注册只会产生误导性的「成功」）。

/// 全局热键动作：决定按到键之后干什么，也决定 portal 里用的稳定 id。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 截图（等价托盘菜单 / --screenshot）
    Screenshot,
    /// 显示并置前主窗口（不额外发 OPEN_UNREAD 事件）
    ShowWindow,
}

// portal_id/description 只有 Linux 的 portal 分支（及单测）会读，
// 非 Linux 构建下补一条 allow 保持零 warning（与 to_portal_trigger 同理）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl Action {
    /// portal GlobalShortcuts 的快捷键 id：跨语言稳定（KDE 把它当作绑定标识，
    /// 改字符串会让用户已有的绑定失效）。
    pub fn portal_id(self) -> &'static str {
        match self {
            Action::Screenshot => "screenshot",
            Action::ShowWindow => "show-window",
        }
    }

    /// portal 绑定描述（本地化；用户在系统设置 / 首次绑定确认框里看到的就是它）。
    fn description(self, lang: &str) -> String {
        match self {
            Action::Screenshot => crate::ui_str(lang, "截图", "Screenshot"),
            Action::ShowWindow => crate::ui_str(lang, "显示主窗口", "Show main window"),
        }
    }
}

/// portal `BindShortcuts` 的一项绑定。字段与 zbus 无关，便于跨平台单测。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 只有 Linux 的 portal 分支会读
#[derive(Debug, PartialEq, Eq)]
pub struct Binding {
    pub id: String,
    pub description: String,
    pub trigger: String,
}

/// 过滤掉空串项（空串 = 该项禁用）。`register_all` 在结果为空时直接返回。
pub fn configured_items(items: &[(String, Action)]) -> Vec<(String, Action)> {
    items
        .iter()
        .filter(|(combo, _)| !combo.trim().is_empty())
        .cloned()
        .collect()
}

/// 构造 portal 绑定表：过滤空串、逐条把组合键转成 XDG 触发器，非法的只丢自己
/// —— 一条坏的 open_hotkey 绝不能把截图热键一起弄失效。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn build_bindings(lang: &str, items: &[(String, Action)]) -> Vec<Binding> {
    configured_items(items)
        .into_iter()
        .filter_map(|(combo, action)| match to_portal_trigger(&combo) {
            Some(trigger) => Some(Binding {
                id: action.portal_id().to_string(),
                description: action.description(lang),
                trigger,
            }),
            None => {
                oim_log!("[hotkey] 非法快捷键（portal 触发器，跳过）：{combo}");
                None
            }
        })
        .collect()
}

/// 监听循环分发：把 portal 回调的 id 映射回动作，只在本次的项里查，未知 id 忽略
/// （例如旧 session 残留或别家注册的同名 id）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn action_for_id(items: &[(String, Action)], id: &str) -> Option<Action> {
    items
        .iter()
        .find(|(_, action)| action.portal_id() == id)
        .map(|(_, action)| *action)
}

/// 修饰键白名单 → XDG 触发器里的修饰键名（Super/Meta/CmdOrCtrl 统一写作 LOGO）。
///
/// 名单以外一律返回 None：旧版把未知修饰键原样大写塞进触发器（"Bogus+Z" → "BOGUS+Z"），
/// 这种串会与合法项混进同一次 BindShortcuts —— 桌面环境可能整批拒绝，那就与
/// 「一条非法项只丢自己」的承诺相反。大小写不敏感（手改配置写 "ctrl+a" 也认），
/// 但只认名单上的名字。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 与 to_portal_trigger 同理
fn portal_modifier(m: &str) -> Option<&'static str> {
    match m.to_ascii_lowercase().as_str() {
        "ctrl" => Some("CTRL"),
        "alt" => Some("ALT"),
        "shift" => Some("SHIFT"),
        "cmdorctrl" | "super" | "meta" => Some("LOGO"),
        _ => None,
    }
}

/// 单字母/数字（前端规范形是大写，xkbcommon 键名是小写）
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_single_ascii_alnum(key: &str) -> bool {
    let mut chars = key.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphanumeric())
}

/// F1..F24，严格匹配：只认规范写法（"F1".."F24"），不做规范化。
/// 前导零不算 —— "F01"/"F001" 解析成 1 若被放行，就会把非规范串带进
/// BindShortcuts（触发器名 xkbcommon 也不认），违背后端「只放行能注册的串」的承诺；
/// "F"（空号）/"F0"/"F25"/"F99" 同理一律拒绝。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_function_key(key: &str) -> bool {
    let digits = match key.chars().next() {
        Some('F') | Some('f') => &key[1..],
        _ => return false,
    };
    // 只接受 1..=24 的十进制写法，且不接受前导零（"01" != "1"）
    match digits.parse::<u8>() {
        Ok(n) => (1..=24).contains(&n) && digits == n.to_string(),
        Err(_) => false,
    }
}

/// 主键 → xkbcommon 键名；只有认得的键才返回 Some。
///
/// 认得三类：前端录制器能产出的特殊键（名字与 xkbcommon 不同时在这里改写）、
/// 单个字母/数字、F1..F24。其余（"Foobar" / "F0" / "F25" / "Å"）返回 None
/// —— 不猜、不放行，理由同 portal_modifier。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn portal_key_name(key: &str) -> Option<String> {
    let named = match key {
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
        // xkbcommon 键名与前端规范名恰好同名：显式列出，而不是「未知就放行」
        "Escape" | "Tab" | "Delete" | "Insert" | "Home" | "End" => key,
        _ => {
            if is_single_ascii_alnum(key) {
                return Some(key.to_ascii_lowercase());
            }
            if is_function_key(key) {
                return Some(key.to_ascii_uppercase());
            }
            return None;
        }
    };
    Some(named.to_string())
}

/// 规范形 "Ctrl+Alt+A" → XDG shortcuts 触发器 "CTRL+ALT+a"
///
/// 只有 Linux 的 portal 分支会调它（Windows/macOS 走插件，那条路径已被 cfg 摘掉），
/// 补一条 allow 让 Task 12/13 的构建保持零 warning。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 只有 Linux 的 portal 分支会调
pub fn to_portal_trigger(combo: &str) -> Option<String> {
    let parts: Vec<&str> = combo.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let (key, mods) = parts.split_last()?;
    // 逐项白名单校验：有一个不认得就整条判非法（绝不把未校验的串带进 BindShortcuts）
    let mut out: Vec<&str> = Vec::with_capacity(mods.len());
    for m in mods {
        out.push(portal_modifier(m)?);
    }
    out.sort();
    let key_name = portal_key_name(key)?;
    Some(format!("{}+{}", out.join("+"), key_name))
}

/// 注册一组全局热键：`items` 是 (规范形组合键, 动作) 列表，空串项 = 禁用。
///
/// 逐条处理：空串项跳过；非法项只跳过自己并记录，绝不拖垮其它有效项（尤其不能
/// 让一条坏的 open_hotkey 把截图热键一起弄失效）。非 Wayland 走插件（每条一次
/// on_shortcut），Wayland 走 portal（一个 session、一次 BindShortcuts 传多条）。
///
/// 与旧版一样：Wayland 分支**不返回** —— portal 的监听循环跑到底；调用方必须
/// 已经在自己线程里（lib.rs setup 里就是 spawn 出去的）。
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))] // lang 只给 portal 用
pub fn register_all(app: &tauri::AppHandle, lang: &str, items: &[(String, Action)]) {
    let configured = configured_items(items);
    if configured.is_empty() {
        // 全部为空：不建 session、不注册，直接返回
        return;
    }
    // Wayland 上 Tauri 插件注册不上（XDG 不允许客户端抢全局按键），走 portal。
    // portal 模块只在 Linux 编译，用 cfg 保证 Windows/macOS 也能编过。
    #[cfg(target_os = "linux")]
    if crate::screenshot::is_wayland() {
        portal::bind_and_listen(app, lang, &configured);
        return;
    }
    for (combo, action) in &configured {
        match plugin::register(app, combo, *action) {
            Ok(()) => oim_log!("[hotkey] 已注册全局热键：{combo}（{action:?}）"),
            Err(e) => oim_log!("[hotkey] 注册失败（跳过该项）：{e}"),
        }
    }
}

/* ---------------- Windows / macOS / X11 ---------------- */

// 三平台共用同一份注册代码：Linux 上插件只在 X11 生效，Wayland 永远走下面的
// portal 分支（见 register_all 里的 is_wayland 判断），所以这里不需要平台分支。
mod plugin {
    use super::Action;
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    pub fn register(app: &tauri::AppHandle, combo: &str, action: Action) -> Result<(), String> {
        let shortcut: tauri_plugin_global_shortcut::Shortcut =
            combo.parse().map_err(|e| format!("非法快捷键 {combo}: {e}"))?;
        let shot_handle = app.clone();
        app.global_shortcut()
            .on_shortcut(shortcut, move |app, _sc, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                match action {
                    Action::Screenshot => {
                        if let Err(e) = crate::screenshot::trigger(&shot_handle) {
                            oim_log!("[hotkey] 热键触发失败：{}", e.message());
                        }
                    }
                    Action::ShowWindow => {
                        // 回调不在主线程：窗口操作必须回主线程（与 activate_from_tray 同）
                        let inner = app.clone();
                        if let Err(e) =
                            app.run_on_main_thread(move || crate::raise_main_window(&inner))
                        {
                            // 主线程已不在（退出中）：记一笔即可，不 panic
                            oim_log!("[hotkey] 主线程调度失败（显示主窗口）：{e}");
                        }
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

    use super::{action_for_id, build_bindings, Action, Binding};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
    const REQ_IFACE: &str = "org.freedesktop.portal.Request";

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
    pub fn bind_and_listen(app: &tauri::AppHandle, lang: &str, items: &[(String, Action)]) {
        let bindings = build_bindings(lang, items);
        if bindings.is_empty() {
            // 连一条合法触发器都构造不出来：不建 session（避免弹无意义的系统确认框）
            oim_log!("[hotkey] 没有可用的 Wayland 快捷键，跳过 portal 绑定");
            return;
        }
        match bind_inner(&bindings) {
            Ok(conn) => {
                let triggers: Vec<&str> = bindings.iter().map(|b| b.trigger.as_str()).collect();
                oim_log!("[hotkey] 全局热键后端：Portal（{}）", triggers.join(", "));
                listen_activated(&conn, app, items);
            }
            Err(e) => oim_log!("[hotkey] Wayland 热键绑定失败：{e}（可用 --screenshot 自行绑定快捷键）"),
        }
    }

    /// CreateSession → BindShortcuts（一次传多条），返回**建会话用的那条连接**：
    /// 调用方一直持有它，session 与 KDE 侧的绑定才不会被回收（见 bind_and_listen 的注释）。
    fn bind_inner(bindings: &[Binding]) -> Result<Connection, String> {
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
        oim_log!("[hotkey] 等待 portal 建会话响应：{handle}");
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

        // 2) 绑定快捷键：一次传多条 (id, opts)（KDE/GNOME 会弹一次系统确认框）
        let token2 = format!("oimbind{}", std::process::id());
        let handle2 = request_path(&conn, &token2)?;
        let req2 = Proxy::new(&conn, DEST, handle2.as_str(), REQ_IFACE).map_err(|e| e.to_string())?;
        let mut signals2 = req2.receive_signal("Response").map_err(|e| e.to_string())?;

        // 每条绑定一个 (id, {description, preferred_trigger})：description 本地化，
        // id 是跨语言稳定的 portal 标识（监听侧按它分发）
        let shortcuts: Vec<(&str, HashMap<&str, Value>)> = bindings
            .iter()
            .map(|b| {
                let mut sc_opts: HashMap<&str, Value> = HashMap::new();
                sc_opts.insert("description", Value::from(b.description.as_str()));
                sc_opts.insert("preferred_trigger", Value::from(b.trigger.as_str()));
                (b.id.as_str(), sc_opts)
            })
            .collect();
        let mut bind_opts: HashMap<&str, Value> = HashMap::new();
        bind_opts.insert("handle_token", Value::from(token2.as_str()));
        let returned2: OwnedObjectPath = gs
            .call("BindShortcuts", &(session_path, shortcuts, "", bind_opts))
            .map_err(|e| format!("绑定快捷键失败: {e}"))?;
        signals2 = retarget(&conn, signals2, &returned2, &handle2)?;
        oim_log!("[hotkey] 等待 portal 绑定响应（首次会弹系统确认框）：{handle2}");
        let (code2, res2) = next_response(&mut signals2)?;
        if code2 != 0 {
            return Err(format!("快捷键绑定被拒绝（响应码 {code2}）"));
        }
        oim_log!("[hotkey] Wayland 热键已绑定（响应 {res2:?}）");
        Ok(conn)
    }

    /// 在同一条连接上收 Activated（定向信号，见 bind_and_listen 的注释）。
    /// 循环直到进程结束 —— 这就是 register_all 的 Wayland 分支不返回的原因。
    fn listen_activated(conn: &Connection, app: &tauri::AppHandle, items: &[(String, Action)]) {
        let proxy = match Proxy::new(conn, DEST, PATH, IFACE) {
            Ok(p) => p,
            Err(e) => {
                oim_log!("[hotkey] 热键监听不可用（{e}）—— 可用 --screenshot 自行绑定快捷键");
                return;
            }
        };
        let mut sig = match proxy.receive_signal("Activated") {
            Ok(s) => s,
            Err(e) => {
                oim_log!("[hotkey] 热键监听不可用（{e}）—— 可用 --screenshot 自行绑定快捷键");
                return;
            }
        };
        for msg in &mut sig {
            let Ok((_session, id, _ts, _opts)): Result<ActivatedBody, _> = msg.body().deserialize()
            else {
                continue;
            };
            match action_for_id(items, &id) {
                Some(Action::Screenshot) => {
                    if let Err(e) = crate::screenshot::trigger(app) {
                        oim_log!("[hotkey] 热键触发失败：{}", e.message());
                    }
                }
                Some(Action::ShowWindow) => {
                    // 回调在 DBus 线程：窗口操作回主线程（与 activate_from_tray 同）。
                    // 复用 raise_main_window，不额外发 OPEN_UNREAD。
                    let inner = app.clone();
                    if let Err(e) =
                        app.run_on_main_thread(move || crate::raise_main_window(&inner))
                    {
                        // 主线程已不在（退出中）：记一笔即可，不 panic
                        oim_log!("[hotkey] 主线程调度失败（显示主窗口）：{e}");
                    }
                }
                // 未知 id（旧 session 残留 / 别家注册的同名 id）忽略
                None => {}
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
            "[hotkey] portal 返回了另一个请求路径 {returned}（预期 {expected}），改挂订阅"
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

    /// 严格校验：未知修饰键 / 未知主键 / 越界 F 键一律被拒。
    /// 旧版会把它们原样塞进触发器（"Bogus+Z" → "BOGUS+Z"、"Ctrl+Foobar" →
    /// "CTRL+Foobar"），坏串与合法项同进一次 BindShortcuts 可能让整批失败。
    #[test]
    fn portal_trigger_rejects_unknown_modifiers_and_keys() {
        assert_eq!(to_portal_trigger("Ctrl+Foobar"), None, "未知主键不能放行");
        assert_eq!(to_portal_trigger("Bogus+Z"), None, "未知修饰键不能放行");
        assert_eq!(to_portal_trigger("Ctrl+F25"), None, "F25 越界");
        assert_eq!(to_portal_trigger("Ctrl+F0"), None, "F0 越界");
        assert_eq!(to_portal_trigger("Ctrl+F99"), None, "F99 越界");
        assert_eq!(to_portal_trigger("Ctrl+F1x"), None, "F 键号不是数字");
        // 前导零是非规范写法：旧版 parse 后当成 F1 放行，会产出 xkbcommon 不认的
        // "CTRL+F01"。严格匹配 F1..F24，不做规范化。
        assert_eq!(to_portal_trigger("Ctrl+F01"), None, "F01 前导零不能放行");
        assert_eq!(to_portal_trigger("Ctrl+F001"), None, "F001 前导零不能放行");
        assert_eq!(to_portal_trigger("Ctrl+F00"), None, "F00 前导零不能放行");
        assert_eq!(to_portal_trigger("Ctrl+Å"), None, "非 ASCII 主键");
        // 主键位置放修饰键名、把修饰键写到主键位置，都不算合法组合
        assert_eq!(to_portal_trigger("Ctrl+Ctrl"), None);
        assert_eq!(to_portal_trigger("Alt+Shift+Control"), None);
        // 一个修饰键非法就整条丢，不能只丢一部分
        assert_eq!(to_portal_trigger("Ctrl+Hyper+Z"), None);
    }

    /// 严格校验不能误伤合法值：单字母/数字、F1..F24、前端能录出的特殊键
    /// （它们都能在设置页里录出来，被拒就会变成「设了不生效」）。
    #[test]
    fn portal_trigger_still_accepts_every_recordable_combo() {
        assert_eq!(to_portal_trigger("Ctrl+A").as_deref(), Some("CTRL+a"));
        // 手改配置写成小写也认（大小写不敏感）
        assert_eq!(to_portal_trigger("ctrl+z").as_deref(), Some("CTRL+z"));
        assert_eq!(to_portal_trigger("Ctrl+5").as_deref(), Some("CTRL+5"));
        assert_eq!(to_portal_trigger("Ctrl+F1").as_deref(), Some("CTRL+F1"));
        assert_eq!(to_portal_trigger("Ctrl+F24").as_deref(), Some("CTRL+F24"));
        assert_eq!(to_portal_trigger("Ctrl+F5").as_deref(), Some("CTRL+F5"));
        // F1..F24 全盘接受（一个都不能少），且保留原键号不做规范化
        for n in 1..=24u8 {
            assert_eq!(
                to_portal_trigger(&format!("Ctrl+F{n}")),
                Some(format!("CTRL+F{n}")),
                "F{n} 是合法功能键，不能被拒"
            );
        }
        assert_eq!(to_portal_trigger("super+Q").as_deref(), Some("LOGO+q"));
        for key in ["Escape", "Tab", "Delete", "Insert", "Home", "End"] {
            assert_eq!(
                to_portal_trigger(&format!("Ctrl+{key}")),
                Some(format!("CTRL+{key}")),
                "{key} 是设置页能录出的键，不能被拒"
            );
        }
    }

    /// portal 绑定表：两条动作各生成一项，id/描述/触发器都对，且描述按语言本地化。
    #[test]
    fn build_bindings_makes_one_entry_per_action() {
        let items = vec![
            ("Ctrl+Alt+Z".to_string(), Action::Screenshot),
            ("Ctrl+Alt+X".to_string(), Action::ShowWindow),
        ];
        let zh = build_bindings("zh-CN", &items);
        assert_eq!(zh.len(), 2);
        assert_eq!(zh[0].id, "screenshot");
        assert_eq!(zh[0].trigger, "ALT+CTRL+z");
        assert_eq!(zh[0].description, "截图");
        assert_eq!(zh[1].id, "show-window");
        assert_eq!(zh[1].trigger, "ALT+CTRL+x");
        assert_eq!(zh[1].description, "显示主窗口");
        // 同一份项换英文语言：只有描述变，id/触发器不变
        let en = build_bindings("en", &items);
        assert_eq!(en[0].description, "Screenshot");
        assert_eq!(en[1].description, "Show main window");
        assert_eq!(en[0].id, zh[0].id);
    }

    /// 空串项（= 禁用）被过滤；全部为空时不构造任何绑定（register_all 也就
    /// 不会建 session、不会注册）。
    #[test]
    fn empty_combos_are_filtered_and_all_empty_register_nothing() {
        let items = vec![
            (String::new(), Action::Screenshot),
            ("   ".to_string(), Action::ShowWindow),
        ];
        assert!(configured_items(&items).is_empty(), "空串项必须被丢弃");
        assert!(
            build_bindings("zh-CN", &items).is_empty(),
            "全部为空时不建任何绑定"
        );
    }

    /// 一条非法项只丢自己：坏的 open_hotkey 不能把截图热键一起弄失效
    /// （to_portal_trigger 要求「修饰键 + 主键」，裸 "Z" 构造不出来）。
    #[test]
    fn one_invalid_combo_does_not_drop_the_others() {
        let items = vec![
            ("Alt+A".to_string(), Action::Screenshot),
            ("Z".to_string(), Action::ShowWindow),
        ];
        let list = build_bindings("zh-CN", &items);
        assert_eq!(list.len(), 1, "非法项应被单独跳过");
        assert_eq!(list[0].id, "screenshot");
        assert_eq!(list[0].trigger, "ALT+a");
    }

    /// 监听分发：按稳定 id 找动作，互不串线；未知 id 一律忽略。
    #[test]
    fn activated_id_dispatches_to_the_right_action_and_ignores_unknown() {
        let items = vec![
            ("Alt+A".to_string(), Action::Screenshot),
            ("Ctrl+Alt+Z".to_string(), Action::ShowWindow),
        ];
        assert_eq!(action_for_id(&items, "screenshot"), Some(Action::Screenshot));
        assert_eq!(action_for_id(&items, "show-window"), Some(Action::ShowWindow));
        // 未知 id（旧 session / 别家注册的同名 id）忽略，不能误触发某个动作
        assert_eq!(action_for_id(&items, "screenshot-2"), None);
        assert_eq!(action_for_id(&items, ""), None);
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
