//! 前后端的**线上契约**：命令名与事件名的唯一真源（架构评审候选 1）。
//!
//! 由来：这两组名字过去在两侧各写一遍 —— Rust 的 `generate_handler!` 与 `emit("…")`
//! 一处，`src/lib/ipc.js` 里 47 个 `invoke("…")` 与事件常量一处，编译器管不到；改名时
//! 没人报错，只有真机上的「点了没反应」。现在：
//!
//! - 事件名由本 module 拥有，Rust 侧一律 `emit(wire::evt::MSG_IN, …)`；
//! - 命令名（`generate_handler!` 只吃函数路径字面量，拼不出常量）在本 module 声明，
//!   由 `every_wire_command_is_registered` 与注册表名单对照，挡改名漂移；
//! - 前端不手抄：`--dump-wire` 生成 `src/lib/wire.generated.js`，
//!   `wire_artifact_is_current` 守着产物不许漂。
//!
//! 注意：`screenshot-done` / `screenshot-copy` **不在这里** —— 它们是遮罩窗口发给主窗口的
//! 窗口间事件（前端 `emitToMain`），不经过后端。

/// 一组线上名字：一次声明同时得到「每名一个常量」与「可遍历的全集」。
macro_rules! wire_names {
    ($mod_name:ident, $doc:literal, $($konst:ident => $wire:literal),+ $(,)?) => {
        #[doc = $doc]
        #[allow(dead_code)] // 命令名常量只有 JS/测试按名字取用，Rust 侧不逐个引用（同 protocol::opt 的做法）
        pub mod $mod_name {
            $(pub const $konst: &str = $wire;)+
            /// 全部取值（生成产物与对照测试用）
            pub const ALL: &[&str] = &[$($wire),+];
        }
    };
}

wire_names!(
    evt,
    "后端 → 前端的事件名（Rust `emit` 的那一侧）。",
    ABSENCE_INFO => "absence-info",
    FILE_PROGRESS => "file-progress",
    MSG_DELIVERED => "msg-delivered",
    MSG_IN => "msg-in",
    MSG_OUT => "msg-out",
    MSG_READ => "msg-read",
    MSG_RECALLED => "msg-recalled",
    MSG_UNLOCKED => "msg-unlocked",
    OPEN_CHAT => "open-chat",
    OPEN_UNREAD => "open-unread",
    USERS_UPDATED => "users-updated",
);

wire_names!(
    cmd,
    "命令名（`#[tauri::command]` 的线上名字）：带模块前缀的注册路径取最后一段。",
    GET_CONFIG => "get_config",
    SAVE_CONFIG => "save_config",
    SET_AUTOSTART => "set_autostart",
    GET_USERS => "get_users",
    REFRESH_USERS => "refresh_users",
    GET_HISTORY => "get_history",
    CLEAR_HISTORY => "clear_history",
    DELETE_CONTACT => "delete_contact",
    SEARCH_HISTORY => "search_history",
    SEND_TEXT => "send_text",
    SEND_FILES => "send_files",
    SEND_CLIPBOARD_IMAGE => "send_clipboard_image",
    STAGE_PASTED_FILE => "stage_pasted_file",
    CLIPBOARD_FILE_PATHS => "clipboard_file_paths",
    CLIPBOARD_IMAGE => "clipboard_image",
    SET_UNREAD => "set_unread",
    NOTIFY_MESSAGE => "notify_message",
    DOWNLOAD_FILE => "download_file",
    READ_IMAGE_DATA => "read_image_data",
    COPY_FILE_AS => "copy_file_as",
    OPEN_IMAGE_VIEWER => "open_image_viewer",
    MARK_READ => "mark_read",
    MARK_OUT_READ => "mark_out_read",
    LIST_SESSIONS => "list_sessions",
    IMPORT_IPMSG_LOG => "import_ipmsg_log",
    SET_ABSENCE => "set_absence",
    RECALL_MESSAGE => "recall_message",
    BROADCAST_MESSAGE => "broadcast_message",
    UNLOCK_MESSAGE => "unlock_message",
    GET_ABSENCE_INFO => "get_absence_info",
    REQUEST_HOSTLIST => "request_hostlist",
    LIST_EMOJIS => "list_emojis",
    EMOJI_SRC_AVAILABLE => "emoji_src_available",
    IMPORT_EMOJI => "import_emoji",
    DELETE_EMOJI => "delete_emoji",
    RENAME_EMOJI => "rename_emoji",
    REORDER_EMOJIS => "reorder_emojis",
    SEND_EMOJI => "send_emoji",
    EXPORT_EMOJI_PACK => "export_emoji_pack",
    INSPECT_EMOJI_PACK => "inspect_emoji_pack",
    IMPORT_EMOJI_PACK => "import_emoji_pack",
    START_SCREENSHOT => "start_screenshot",
    SHOT_OVERLAY_READY => "shot_overlay_ready",
    SHOT_IMAGE => "shot_image",
    CLOSE_SHOT_OVERLAYS => "close_shot_overlays",
    SAVE_SHOT_PNG => "save_shot_png",
    COPY_IMAGE_TO_CLIPBOARD => "copy_image_to_clipboard",
);

/// 每个事件 payload 的字段名（**前端读的键**）。
///
/// `emit` 点用 `json!` 拼载荷，形状没法由类型保证，靠 `event_payload_keys_are_declared`
/// 对着真实发射结果核：**实际键必须落在声明里**。`file-progress` 有五种发法（进度 /
/// 完成 / 失败 / 加密流），这里取并集 —— 改名的键会落到并集之外被抓住，而可选字段
/// （path / error / enc）不必为每种组合各写一条。
pub mod fields {
    pub const ABSENCE_INFO: &[&str] = &["key", "text"];
    pub const FILE_PROGRESS: &[&str] = &[
        "key", "pkt", "file_id", "transferred", "total", "done", "path", "error", "enc",
    ];
    pub const MSG_DELIVERED: &[&str] = &["key", "pkt"];
    pub const MSG_IN: &[&str] = &["key", "msg", "resend"];
    pub const MSG_OUT: &[&str] = &["key", "msg"];
    pub const MSG_READ: &[&str] = &["key", "pkt"];
    pub const MSG_RECALLED: &[&str] = &["key", "pkt", "peer"];
    pub const MSG_UNLOCKED: &[&str] = &["key", "pkt"];
    /// `open-unread` 只叫人「跳过去」，不带载荷（`emit(…, ())`）
    pub const OPEN_UNREAD: &[&str] = &[];
    pub const OPEN_CHAT: &[&str] = &["key"];
    /// `users-updated` 只表示「列表变了」，不带字段
    pub const USERS_UPDATED: &[&str] = &[];
}

/// 事件名 → payload 字段声明（产物生成与对照测试共用一张表）。
pub fn declared_fields(event: &str) -> Option<&'static [&'static str]> {
    Some(match event {
        evt::ABSENCE_INFO => fields::ABSENCE_INFO,
        evt::FILE_PROGRESS => fields::FILE_PROGRESS,
        evt::MSG_DELIVERED => fields::MSG_DELIVERED,
        evt::MSG_IN => fields::MSG_IN,
        evt::MSG_OUT => fields::MSG_OUT,
        evt::MSG_READ => fields::MSG_READ,
        evt::MSG_RECALLED => fields::MSG_RECALLED,
        evt::MSG_UNLOCKED => fields::MSG_UNLOCKED,
        evt::OPEN_UNREAD => fields::OPEN_UNREAD,
        evt::OPEN_CHAT => fields::OPEN_CHAT,
        evt::USERS_UPDATED => fields::USERS_UPDATED,
        _ => return None,
    })
}

/// 生成给前端的 JS 产物。**产物进仓**（`src/lib/wire.generated.js`），
/// `wire_artifact_is_current` 守着它不许漂 —— 改了名字忘了重新生成，测试就红。
pub fn js_artifact() -> String {
    let mut s = String::from(
        "/**\n * 本文件由 `cargo run --bin open-ipmsg -- --dump-wire` 生成，不要手改。\n *\n * 单一真源：src-tauri/src/wire.rs（`cargo test --lib wire_artifact_is_current` 守着它不漂）。\n */\nexport const WIRE = {\n  /** 后端 → 前端的事件名（Rust emit 的那一侧） */\n  evt: {\n",
    );
    for name in evt::ALL {
        s.push_str(&format!("    '{name}': '{name}',\n"));
    }
    s.push_str("  },\n  /** 命令名（#[tauri::command] 的线上名字），键即线上名 */\n  cmd: {\n");
    for name in cmd::ALL {
        s.push_str(&format!("    {name}: '{name}',\n"));
    }
    s.push_str("  },\n  /** 每个事件 payload 的字段（前端读的键；file-progress 是并集，可选字段靠 ⊆ 约束） */\n  fields: {\n");
    for name in evt::ALL {
        let list: Vec<String> = declared_fields(name)
            .unwrap_or(&[])
            .iter()
            .map(|f| format!("'{f}'"))
            .collect();
        s.push_str(&format!("    '{name}': [{}],\n", list.join(", ")));
    }
    s.push_str("  },\n};\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn repo_file(rel: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {}: {e}", p.display()))
    }

    /// 产物必须与仓里那份逐字节一致：改了名字却忘了 `--dump-wire`，这条红。
    #[test]
    fn wire_artifact_is_current() {
        assert_eq!(
            repo_file("../src/lib/wire.generated.js"),
            js_artifact(),
            "wire 产物过期：跑 cargo run --bin open-ipmsg -- --dump-wire"
        );
    }

    /// 命令名的真源在本 module，注册表在 lib.rs 的 `generate_handler!`（只吃函数路径
    /// 字面量，拼不出常量）—— 这条测试把两侧对上，挡住改名漂移。
    #[test]
    fn every_wire_command_is_registered() {
        let lib = repo_file("src/lib.rs");
        let block = lib
            .split_once("generate_handler![")
            .expect("lib.rs 里找不到 generate_handler!")
            .1;
        // 注册表以 `])` 收尾，且条目里不会出现 `])`，直接切第一处即可
        let block = block.split("])").next().unwrap_or(block);
        let registered: BTreeSet<&str> = block
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(|t| t.rsplit("::").next().unwrap_or(t))
            .collect();
        let declared: BTreeSet<&str> = cmd::ALL.iter().copied().collect();
        assert_eq!(declared, registered, "wire::cmd 与 generate_handler! 名单不一致");
    }

    /// 事件名不许再以裸字符串出现 —— 否则真源又分叉成两份。
    #[test]
    fn emit_sites_use_constants_only() {
        for f in ["src/lib.rs", "src/net.rs"] {
            let text = repo_file(f);
            assert!(
                !text.contains("emit(\""),
                "{f} 里还有裸字符串事件名，改成 wire::evt::* 常量"
            );
        }
    }
}
