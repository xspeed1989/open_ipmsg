//! 自定义表情包：本地表情库的索引/增删改排序、发送与「表情包」导入导出。
//!
//! 存储布局（与 config.json / hidden_contacts.json 同一数据目录）：
//!
//! ```text
//! <data_dir>/emojis/
//!   registry.json           索引：{ version, emojis: [{ id, name, file, size, sha, added_at }] }
//!   <id>.<ext>              图片本体，导入即固化，重命名/排序不动文件名
//!   tmp-<pid>/              导入包时的临时解压目录，用完即删
//! ```
//!
//! 发送不走新协议：把表情复制成官方「粘贴图片」的 `ipmsgclip_s_<id>_0.<ext>`
//! 再按 FILE_CLIPBOARD(0x20)+CLIPBOARDPOS 公告（见 net::send_message_multi_opts），
//! 官方 IPMsg / iptux 等对端都能内嵌收到。
//!
//! 表情包是标准 zip（后缀 .ipmojis，可用系统解压工具直接打开）：
//!
//! ```text
//! emoji-pack.json          { format, version, name, created_at, emojis: [{ name, file, size }] }
//! images/<file>
//! README.txt
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::SharedState;
use tauri::State;

/// 索引文件名（放在 emojis/ 目录内，与图片同处一处便于整体搬迁/备份）
const REGISTRY_FILE: &str = "registry.json";
/// 表情库目录名
const DIR_NAME: &str = "emojis";
/// 单张表情上限：16MB（覆盖动图/高清截图）
pub const MAX_EMOJI_BYTES: u64 = 16 * 1024 * 1024;
/// 一个表情包内所有图片累计上限
pub const MAX_PACK_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// 一个表情包内的条目上限
pub const MAX_PACK_ENTRIES: usize = 500;
/// 表情库容量上限（防止无限增长）
pub const MAX_EMOJIS: usize = 2000;
/// 名字长度上限（字符数）
const MAX_NAME_CHARS: usize = 32;
/// 包内清单名与图片目录名
const PACK_MANIFEST: &str = "emoji-pack.json";
const PACK_IMAGES_DIR: &str = "images";
const PACK_FORMAT: &str = "open-ipmsg-emoji-pack";
const PACK_VERSION: u32 = 1;

/// 文件名序号（同一毫秒内多次导入也要拿到不同 id）
static SEQ: AtomicU64 = AtomicU64::new(0);

/* ================= 数据模型 ================= */

fn default_version() -> u32 {
    1
}

/// 表情库中的一张表情
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EmojiEntry {
    pub id: String,
    pub name: String,
    /// 相对 emojis/ 的文件名
    pub file: String,
    #[serde(default)]
    pub size: u64,
    /// 内容指纹（长度 + 首尾 4KB 的 SHA-256），导入去重用
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub added_at: u64,
    /// 最近一次发送时缓存目录里的副本名（ipmsgclip_s_*.ext）。
    /// 前端据此把「自己发出的这条消息」认成表情并按缩略渲染 —— 缓存副本是
    /// 逐字节复制，但文件名与库内文件名不同，只能记下映射。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cache_file: String,
}

/// 索引文件
#[derive(Serialize, Deserialize, Clone, Debug)]
struct EmojiRegistry {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    emojis: Vec<EmojiEntry>,
}

impl Default for EmojiRegistry {
    fn default() -> Self {
        Self {
            version: 1,
            emojis: Vec::new(),
        }
    }
}

/// 表情库读写锁（与 AppState 的其他持久化同一套「内存态 + 落盘」思路）
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

/* ================= 纯函数工具 ================= */

/// 按文件头魔数判定图片类型（不看扩展名，防止伪装成 png 的可执行文件入库）
pub fn detect_image_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("png");
    }
    if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return Some("jpg");
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return Some("gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    if bytes.len() >= 2 && &bytes[..2] == b"BM" {
        return Some("bmp");
    }
    None
}

/// 名字清洗：去首尾空白与控制字符、去掉换行、限长；为空时回落默认名
pub fn clean_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return "表情".to_string();
    }
    cleaned.chars().take(MAX_NAME_CHARS).collect()
}

/// 由文件名（或路径）推默认表情名：去掉目录与扩展名
pub fn name_from_path(path: &str) -> String {
    let base = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let stem = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => &base[..],
    };
    clean_name(stem)
}

/// 包内条目名是否安全：拒绝绝对路径、盘符、`..` 穿越与空名
pub fn safe_entry_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 200 {
        return false;
    }
    if name.starts_with('/') || name.starts_with('\\') || name.contains(':') {
        return false;
    }
    if name.contains('\\') {
        return false;
    }
    name.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// 内容指纹：长度 + 首尾各 4KB 的 SHA-256（不必读全文件，大图导入不卡）
pub fn content_sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update((bytes.len() as u64).to_le_bytes());
    let head = &bytes[..bytes.len().min(4096)];
    let tail_start = bytes.len().saturating_sub(4096);
    h.update(head);
    h.update(&bytes[tail_start..]);
    format!("{:x}", h.finalize())
}

/// 生成表情 id：毫秒时间戳 + 进程内序号（同一毫秒批量导入也不撞）
fn next_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{ms:x}{n:02x}")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/* ================= 索引读写 ================= */

pub fn emoji_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR_NAME)
}

fn registry_path(data_dir: &Path) -> PathBuf {
    emoji_dir(data_dir).join(REGISTRY_FILE)
}

/// 读索引；文件损坏或不存在时按空库处理（不让一条坏 JSON 卡死整个表情功能）
fn read_registry(data_dir: &Path) -> EmojiRegistry {
    match std::fs::read(registry_path(data_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => EmojiRegistry::default(),
    }
}

/// 写索引（先写临时文件再改名，避免写一半掉电留下半截 JSON）
fn write_registry(data_dir: &Path, reg: &EmojiRegistry) -> Result<(), String> {
    let dir = emoji_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建表情目录失败：{e}"))?;
    let bytes = serde_json::to_vec_pretty(reg).map_err(|e| format!("序列化失败：{e}"))?;
    let tmp = dir.join("registry.json.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("写入索引失败：{e}"))?;
    std::fs::rename(&tmp, registry_path(data_dir)).map_err(|e| format!("保存索引失败：{e}"))
}

/// 剔除图片文件已丢失的条目（用户手工删了文件、或同步冲突）
fn prune_missing(data_dir: &Path, reg: &mut EmojiRegistry) -> bool {
    let dir = emoji_dir(data_dir);
    let before = reg.emojis.len();
    reg.emojis.retain(|e| dir.join(&e.file).is_file());
    before != reg.emojis.len()
}

/// 列出表情（只读；自动剔除失效项并回写索引）
fn load_emojis(data_dir: &Path) -> Result<Vec<EmojiEntry>, String> {
    let _guard = REGISTRY_LOCK.lock().map_err(|_| "表情库锁异常")?;
    let mut reg = read_registry(data_dir);
    if prune_missing(data_dir, &mut reg) {
        let _ = write_registry(data_dir, &reg);
    }
    Ok(reg.emojis)
}

/* ================= 导入单张 ================= */

/// 导入结果：成功入库的条目 + 跳过项（名字，原因）
type ImportOutcome = (Vec<EmojiEntry>, Vec<(String, String)>);

/// 把一批源文件导入表情库，返回 (成功列表, 跳过原因列表)。
///
/// 校验：存在且是普通文件、≤16MB、魔数判定为图片、指纹去重、库容量上限。
fn import_files(data_dir: &Path, sources: &[(String, Vec<u8>)]) -> Result<ImportOutcome, String> {
    let dir = emoji_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建表情目录失败：{e}"))?;
    let mut imported: Vec<EmojiEntry> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    {
        let _guard = REGISTRY_LOCK.lock().map_err(|_| "表情库锁异常")?;
        let mut reg = read_registry(data_dir);
        prune_missing(data_dir, &mut reg);
        let mut known: HashSet<String> = reg.emojis.iter().map(|e| e.sha.clone()).collect();

        for (label, bytes) in sources {
            if bytes.len() as u64 > MAX_EMOJI_BYTES {
                skipped.push((
                    label.clone(),
                    format!("超过 {}MB 上限", MAX_EMOJI_BYTES / 1024 / 1024),
                ));
                continue;
            }
            let Some(kind) = detect_image_kind(bytes) else {
                skipped.push((label.clone(), "不是支持的图片格式".to_string()));
                continue;
            };
            let sha = content_sha(bytes);
            if known.contains(&sha) {
                skipped.push((label.clone(), "表情库里已有相同图片".to_string()));
                continue;
            }
            if reg.emojis.len() + imported.len() >= MAX_EMOJIS {
                skipped.push((label.clone(), "表情库数量已达上限".to_string()));
                continue;
            }
            let id = next_id();
            let file = format!("{id}.{kind}");
            std::fs::write(dir.join(&file), bytes).map_err(|e| format!("写入表情失败：{e}"))?;
            known.insert(sha.clone());
            imported.push(EmojiEntry {
                id,
                name: name_from_path(label),
                file,
                size: bytes.len() as u64,
                sha,
                added_at: now_secs(),
                cache_file: String::new(),
            });
        }

        if !imported.is_empty() {
            reg.emojis.extend(imported.iter().cloned());
            write_registry(data_dir, &reg)?;
        }
    }

    Ok((imported, skipped))
}

/* ================= 表情包（zip） ================= */

/// 包清单里的一个条目
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackItem {
    #[serde(default)]
    pub name: String,
    /// 包内相对路径，形如 images/xxx.png
    pub file: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct PackManifest {
    #[serde(default)]
    format: String,
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    emojis: Vec<PackItem>,
}

fn zip_error<E: std::fmt::Display>(e: E) -> String {
    format!("表情包处理失败：{e}")
}

fn write_zip_entry<W: Write + std::io::Seek>(
    zw: &mut zip::ZipWriter<W>,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zw.start_file(name, opts).map_err(zip_error)?;
    zw.write_all(bytes).map_err(zip_error)
}

/// 导出表情包：ids 为空 = 导出全部；否则只导出命中的那些。
/// 返回实际写入的表情数量。
pub fn export_pack(
    data_dir: &Path,
    ids: &[String],
    dest: &Path,
    pack_name: &str,
) -> Result<usize, String> {
    let dir = emoji_dir(data_dir);
    let all = load_emojis(data_dir)?;
    let picked: Vec<EmojiEntry> = if ids.is_empty() {
        all
    } else {
        let want: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        all.into_iter().filter(|e| want.contains(e.id.as_str())).collect()
    };
    if picked.is_empty() {
        return Err("没有可导出的表情".into());
    }

    // 保留原名里的扩展名（用于分享包里一眼看懂），条目名仍用 images/ 前缀
    let mut items: Vec<(PackItem, PathBuf)> = Vec::new();
    for (i, e) in picked.iter().enumerate() {
        let src = dir.join(&e.file);
        if !src.is_file() {
            continue;
        }
        let ext = Path::new(&e.file)
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("png");
        let entry = format!(
            "{PACK_IMAGES_DIR}/{:03}_{}.{ext}",
            i + 1,
            sanitize_file_stem(&e.name)
        );
        items.push((
            PackItem {
                name: e.name.clone(),
                file: entry,
                size: e.size,
            },
            src,
        ));
    }
    if items.is_empty() {
        return Err("没有可导出的表情".into());
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let file = std::fs::File::create(dest).map_err(|e| format!("创建表情包失败：{e}"))?;
    let mut zw = zip::ZipWriter::new(file);

    let manifest = PackManifest {
        format: PACK_FORMAT.to_string(),
        version: PACK_VERSION,
        name: clean_name(pack_name),
        created_at: now_secs(),
        emojis: items.iter().map(|(it, _)| it.clone()).collect(),
    };
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("序列化清单失败：{e}"))?;
    // 清单放第一个条目：即使包被截断，也能先看到这是什么东西
    write_zip_entry(&mut zw, PACK_MANIFEST, &manifest_bytes)?;
    write_zip_entry(
        &mut zw,
        "README.txt",
        format!(
            "Open IPMsg 自定义表情包（{} 张）\n\
             本文件是标准 zip：emoji-pack.json 是清单，images/ 下是原始图片，可直接解压取用。\n\
             在 Open IPMsg 的表情面板「自定义 → 导入表情包」里选择本文件即可整包导入。\n",
            items.len()
        )
        .as_bytes(),
    )?;
    for (item, src) in &items {
        let bytes = std::fs::read(src).map_err(|e| format!("读取表情失败：{e}"))?;
        write_zip_entry(&mut zw, &item.file, &bytes)?;
    }
    zw.finish().map_err(zip_error)?;
    Ok(items.len())
}

/// 文件名片段清洗：只留字母数字与常见符号，避免包里出现奇怪路径
fn sanitize_file_stem(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .collect();
    let s = s.trim().replace(' ', "_");
    if s.is_empty() {
        "emoji".to_string()
    } else {
        s.chars().take(24).collect()
    }
}

/// 包内一个条目在导入前的检查结果
#[derive(Serialize, Clone, Debug)]
pub struct PackInspectItem {
    /// 包内相对路径（导入时按它回查）
    pub file: String,
    pub name: String,
    pub size: u64,
    /// 魔数判定的图片类型；None = 非法
    pub kind: Option<String>,
    /// 与本地表情库重复（按指纹）
    pub duplicate: bool,
    /// 非法原因；None = 可导入
    pub problem: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PackInspect {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub created_at: u64,
    /// 清单条目数（含非法项）
    pub total: usize,
    /// 可导入条目数（不含重复与非法）
    pub importable: usize,
    pub items: Vec<PackInspectItem>,
}

/// 把包文件复制到临时目录再打开：直接句柄读包时若中途被替换/截断，
/// 校验过程会读到半截数据；复制一份可保证「检查」与「导入」看到同一个包。
struct StagedPack {
    path: PathBuf,
    tmp_dir: PathBuf,
}

impl Drop for StagedPack {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp_dir);
    }
}

fn stage_pack(data_dir: &Path, src: &Path) -> Result<StagedPack, String> {
    if !src.is_file() {
        return Err("表情包文件不存在".into());
    }
    let tmp_dir = emoji_dir(data_dir).join(format!("tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("创建临时目录失败：{e}"))?;
    let path = tmp_dir.join("pack.zip");
    std::fs::copy(src, &path).map_err(|e| format!("读取表情包失败：{e}"))?;
    Ok(StagedPack { path, tmp_dir })
}

/// 解析包清单；返回 (清单, zip 读取器)
fn open_pack(
    pack: &StagedPack,
) -> Result<(PackManifest, zip::ZipArchive<std::fs::File>), String> {
    let f = std::fs::File::open(&pack.path).map_err(|e| format!("读取表情包失败：{e}"))?;
    let mut za = zip::ZipArchive::new(f).map_err(|_| "不是有效的表情包（zip 解析失败）".to_string())?;
    let manifest = {
        let mut raw = String::new();
        let mut entry = za
            .by_name(PACK_MANIFEST)
            .map_err(|_| "表情包缺少 emoji-pack.json 清单".to_string())?;
        entry
            .read_to_string(&mut raw)
            .map_err(|e| format!("读取清单失败：{e}"))?;
        serde_json::from_str::<PackManifest>(&raw)
            .map_err(|_| "表情包清单格式不正确".to_string())?
    };
    if manifest.format != PACK_FORMAT {
        return Err("这不是 Open IPMsg 的表情包".into());
    }
    if manifest.version > PACK_VERSION {
        return Err(format!(
            "表情包版本 {} 高于本版本支持的 {}，请升级应用",
            manifest.version, PACK_VERSION
        ));
    }
    Ok((manifest, za))
}

/// 检查表情包内容（不解压落盘），供导入前预览
pub fn inspect_pack(data_dir: &Path, src: &Path) -> Result<PackInspect, String> {
    let pack = stage_pack(data_dir, src)?;
    let (manifest, mut za) = open_pack(&pack)?;
    let local: HashSet<String> = load_emojis(data_dir)?
        .into_iter()
        .map(|e| e.sha)
        .collect();

    let mut items: Vec<PackInspectItem> = Vec::new();
    let mut total_bytes: u64 = 0;
    for (i, it) in manifest.emojis.iter().enumerate() {
        if i >= MAX_PACK_ENTRIES {
            break;
        }
        let mut item = PackInspectItem {
            file: it.file.clone(),
            name: if it.name.trim().is_empty() {
                name_from_path(&it.file)
            } else {
                clean_name(&it.name)
            },
            size: it.size,
            kind: None,
            duplicate: false,
            problem: None,
        };
        if !safe_entry_name(&it.file) {
            item.problem = Some("包内路径不合法".into());
            items.push(item);
            continue;
        }
        // 以实际解出的长度为准（清单里的 size 只是提示，可被伪造）；
        // take(上限+1) 防止伪造的巨型条目把内存吃光
        let mut bytes: Vec<u8> = Vec::new();
        match za.by_name(&it.file) {
            Ok(mut entry) => {
                if let Err(e) = entry
                    .by_ref()
                    .take(MAX_EMOJI_BYTES + 1)
                    .read_to_end(&mut bytes)
                {
                    item.problem = Some(format!("读取失败：{e}"));
                    items.push(item);
                    continue;
                }
            }
            Err(_) => {
                item.problem = Some("包内缺少该文件".into());
                items.push(item);
                continue;
            }
        }
        item.size = bytes.len() as u64;
        match detect_image_kind(&bytes) {
            Some(kind) => item.kind = Some(kind.to_string()),
            None => {
                item.problem = Some("不是支持的图片格式".into());
                items.push(item);
                continue;
            }
        }
        if bytes.len() as u64 > MAX_EMOJI_BYTES {
            item.problem = Some(format!("超过 {}MB 上限", MAX_EMOJI_BYTES / 1024 / 1024));
            items.push(item);
            continue;
        }
        total_bytes += bytes.len() as u64;
        if total_bytes > MAX_PACK_TOTAL_BYTES {
            item.problem = Some("包内图片总大小超限".into());
            items.push(item);
            continue;
        }
        if local.contains(&content_sha(&bytes)) {
            item.duplicate = true;
            item.problem = Some("表情库里已有相同图片".into());
        }
        items.push(item);
    }

    let importable = items
        .iter()
        .filter(|i| i.problem.is_none() && i.kind.is_some())
        .count();
    Ok(PackInspect {
        format: manifest.format,
        version: manifest.version,
        name: if manifest.name.trim().is_empty() {
            "未命名表情包".to_string()
        } else {
            manifest.name
        },
        created_at: manifest.created_at,
        total: items.len(),
        importable,
        items,
    })
}

/// 按选择导入表情包；files 为空 = 导入全部可导入项。
/// 返回 (新增数, 跳过列表)
pub fn import_pack(
    data_dir: &Path,
    src: &Path,
    files: &[String],
) -> Result<(usize, Vec<(String, String)>), String> {
    let pack = stage_pack(data_dir, src)?;
    let (manifest, mut za) = open_pack(&pack)?;
    let want: Option<HashSet<&str>> = if files.is_empty() {
        None
    } else {
        Some(files.iter().map(|s| s.as_str()).collect())
    };

    let mut picked: Vec<(String, Vec<u8>)> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut total_bytes: u64 = 0;
    for (i, it) in manifest.emojis.iter().enumerate() {
        if i >= MAX_PACK_ENTRIES {
            skipped.push((it.file.clone(), "超出包内条目上限".to_string()));
            continue;
        }
        if let Some(w) = &want {
            if !w.contains(it.file.as_str()) {
                continue;
            }
        }
        let label = if it.name.trim().is_empty() {
            name_from_path(&it.file)
        } else {
            clean_name(&it.name)
        };
        if !safe_entry_name(&it.file) {
            skipped.push((it.file.clone(), "包内路径不合法".to_string()));
            continue;
        }
        let mut bytes = Vec::new();
        match za.by_name(&it.file) {
            Ok(mut entry) => {
                if let Err(e) = entry
                    .by_ref()
                    .take(MAX_EMOJI_BYTES + 1)
                    .read_to_end(&mut bytes)
                {
                    skipped.push((label, format!("读取失败：{e}")));
                    continue;
                }
            }
            Err(_) => {
                skipped.push((label, "包内缺少该文件".to_string()));
                continue;
            }
        }
        if bytes.len() as u64 > MAX_EMOJI_BYTES {
            skipped.push((label, format!("超过 {}MB 上限", MAX_EMOJI_BYTES / 1024 / 1024)));
            continue;
        }
        total_bytes += bytes.len() as u64;
        if total_bytes > MAX_PACK_TOTAL_BYTES {
            skipped.push((label, "包内图片总大小超限".to_string()));
            continue;
        }
        // 用包内条目名做默认名来源（保留原始文件名信息）
        picked.push((label, bytes));
    }

    let (imported, mut more) = import_files(data_dir, &picked)?;
    skipped.append(&mut more);
    Ok((imported.len(), skipped))
}

/* ================= 发送 ================= */

/// 魔数判定结果 → MIME。
///
/// 必须与 net::stage_clipboard_image 支持的 MIME 集合完全一致：
/// 表情落盘要按官方「粘贴图片」公告，那里只认这五种；漏一种就会在发送时才报错，
/// 所以这里返回 Option，由调用方在落盘前给出明确失败（并有单测锁住）。
pub fn mime_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "png" => Some("image/png"),
        "jpg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// 发送自定义表情：把库里的图复制成官方「粘贴图片」命名后按附件发出。
/// 返回送出的消息记录数组（与 send_files 一致）。
pub async fn send_one(
    ctx: &crate::SharedCtx,
    key: &str,
    id: &str,
    text: &str,
) -> Result<serde_json::Value, String> {
    let st = &ctx.st;
    let dir = emoji_dir(&st.data_dir);
    let entry = load_emojis(&st.data_dir)?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| "表情不存在（可能已被删除）".to_string())?;
    let src = dir.join(&entry.file);
    let bytes = std::fs::read(&src).map_err(|e| format!("读取表情失败：{e}"))?;
    let kind = detect_image_kind(&bytes).ok_or_else(|| "表情文件已损坏".to_string())?;
    let mime = mime_for_kind(kind).ok_or_else(|| format!("暂不支持发送这种图片：{kind}"))?;
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let path = crate::net::stage_clipboard_image(&st.data_dir, &b64, mime)?;
    // 记下这次发送用的缓存副本名：前端据此把发出的消息认成表情（缩略渲染），
    // 也用于「添加到表情」时识别自己刚发过的图
    if let Some(staged) = path.file_name().and_then(|s| s.to_str()) {
        let _guard = REGISTRY_LOCK.lock().map_err(|_| "表情库锁异常")?;
        let mut reg = read_registry(&st.data_dir);
        for e in reg.emojis.iter_mut() {
            if e.id == id {
                e.cache_file = staged.to_string();
            }
        }
        let _ = write_registry(&st.data_dir, &reg);
    }
    Ok(serde_json::json!(
        crate::net::send_message_multi_opts(
            ctx,
            key,
            text,
            vec![path.to_string_lossy().into_owned()],
            crate::net::MsgSendOpts {
                clip_pos: Some(0),
                ..Default::default()
            },
        )
        .await?
    ))
}

/* ================= Tauri 命令 ================= */

/// 本地是否已有可用的图片文件；path 不可用时按 name 在下载目录里兜底找。
///
/// 聊天里的附件记录不可全信：官方客户端历史导入的附件只有文件名没有内容；
/// 下载已完整落盘、追踪状态却记成 failed 的情况也真实存在（2026-09 真机实测：
/// 文件字节数与公告一致、PNG 以 IEND+CRC 正常收尾，状态却是 failed）。
/// 前端在「添加到表情」前先问这个，否则会把「本地明明有」判成「还没下载」。
///
/// name 只取最后一段，杜绝 `../` 之类的路径穿越。
#[tauri::command]
pub async fn emoji_src_available(
    path: String,
    name: Option<String>,
    st: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    Ok(src_available_payload(&path, name.as_deref(), &st.data_dir))
}

/// emoji_src_available 的实际逻辑（单独拆出来便于单测）
fn src_available_payload(path: &str, name: Option<&str>, data_dir: &Path) -> serde_json::Value {
    let hit = |p: &Path| {
        serde_json::json!({
            "ok": true,
            "size": std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
            "path": p.to_string_lossy(),
        })
    };
    if !path.is_empty() {
        let p = Path::new(path);
        if p.is_file() {
            return hit(p);
        }
    }
    // 记录里的路径不可用 → 按文件名到下载目录再找一个（用户改过下载目录、
    // 或记录里的路径是历史遗留的别处路径时都能救回来）
    if let Some(n) = name {
        let base = n.rsplit(['/', '\\']).next().unwrap_or("").trim();
        if !base.is_empty() && base != "." && base != ".." {
            let candidate = download_dir(data_dir).join(base);
            if candidate.is_file() {
                return hit(&candidate);
            }
        }
    }
    serde_json::json!({ "ok": false, "size": 0, "path": "" })
}

/// 下载目录：config.json 的 download_dir，未设置时回落 `<data_dir>/接收文件`
/// （与 download_file_task 的落盘位置一致）
fn download_dir(data_dir: &Path) -> PathBuf {
    std::fs::read(data_dir.join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v.get("download_dir")
                .and_then(|d| d.as_str())
                .map(str::to_string)
        })
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("接收文件"))
}

/// 列出自定义表情（剔除失效项）
#[tauri::command]
pub async fn list_emojis(st: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let list = load_emojis(&st.data_dir)?;
    Ok(serde_json::json!({ "emojis": entries_json(&st.data_dir, list) }))
}

/// 给条目补上绝对路径：前端要拿它调 read_image_data 取缩略图。
/// 单独拼 JSON 而不是改 EmojiEntry，避免把「展示用」字段写进索引文件。
fn entries_json(data_dir: &Path, list: Vec<EmojiEntry>) -> Vec<serde_json::Value> {
    let dir = emoji_dir(data_dir);
    list.into_iter()
        .map(|e| {
            let abs = dir.join(&e.file).to_string_lossy().into_owned();
            let mut v = serde_json::to_value(&e).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("abs".to_string(), serde_json::Value::String(abs));
            }
            v
        })
        .collect()
}

/// 从本地文件导入表情（前端只传路径，读盘/校验都在后端做，大图不经 IPC）
#[tauri::command]
pub async fn import_emoji(
    paths: Vec<String>,
    st: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let mut sources: Vec<(String, Vec<u8>)> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    for p in &paths {
        let path = Path::new(p);
        if !path.is_file() {
            skipped.push((p.clone(), "文件不存在".to_string()));
            continue;
        }
        match std::fs::metadata(path) {
            Ok(m) if m.len() > MAX_EMOJI_BYTES => {
                skipped.push((
                    p.clone(),
                    format!("超过 {}MB 上限", MAX_EMOJI_BYTES / 1024 / 1024),
                ));
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                skipped.push((p.clone(), format!("读取失败：{e}")));
                continue;
            }
        }
        match std::fs::read(path) {
            Ok(bytes) => sources.push((name_from_path(p), bytes)),
            Err(e) => skipped.push((p.clone(), format!("读取失败：{e}"))),
        }
    }
    let (imported, mut more) = import_files(&st.data_dir, &sources)?;
    skipped.append(&mut more);
    Ok(serde_json::json!({
        "imported": entries_json(&st.data_dir, imported),
        "skipped": skipped.into_iter().map(|(n, r)| serde_json::json!({ "name": n, "reason": r })).collect::<Vec<_>>(),
    }))
}

/// 删除表情（文件 + 索引）
#[tauri::command]
pub async fn delete_emoji(
    ids: Vec<String>,
    st: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let dir = emoji_dir(&st.data_dir);
    let want: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    {
        let _guard = REGISTRY_LOCK.lock().map_err(|_| "表情库锁异常")?;
        let mut reg = read_registry(&st.data_dir);
        let mut removed = 0usize;
        reg.emojis.retain(|e| {
            if want.contains(e.id.as_str()) {
                let _ = std::fs::remove_file(dir.join(&e.file));
                removed += 1;
                false
            } else {
                true
            }
        });
        write_registry(&st.data_dir, &reg)?;
        Ok(serde_json::json!({ "removed": removed }))
    }
}

/// 重命名表情
#[tauri::command]
pub async fn rename_emoji(
    id: String,
    name: String,
    st: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let clean = clean_name(&name);
    let _guard = REGISTRY_LOCK.lock().map_err(|_| "表情库锁异常")?;
    let mut reg = read_registry(&st.data_dir);
    let mut hit = false;
    for e in reg.emojis.iter_mut() {
        if e.id == id {
            e.name = clean.clone();
            hit = true;
        }
    }
    if !hit {
        return Err("表情不存在".into());
    }
    write_registry(&st.data_dir, &reg)?;
    Ok(serde_json::json!({ "name": clean }))
}

/// 重排表情（拖拽排序）：按给定 id 顺序排列，未出现的排在后面保持原相对顺序
#[tauri::command]
pub async fn reorder_emojis(
    ids: Vec<String>,
    st: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let _guard = REGISTRY_LOCK.lock().map_err(|_| "表情库锁异常")?;
    let mut reg = read_registry(&st.data_dir);
    let pos: std::collections::HashMap<&str, usize> =
        ids.iter().enumerate().map(|(i, s)| (s.as_str(), i)).collect();
    let mut ordered: Vec<EmojiEntry> = Vec::with_capacity(reg.emojis.len());
    let mut rest: Vec<EmojiEntry> = Vec::new();
    let total = reg.emojis.len();
    // 先给到齐的 id 排序
    let mut head: Vec<(usize, EmojiEntry)> = Vec::new();
    for e in reg.emojis.drain(..) {
        match pos.get(e.id.as_str()) {
            Some(i) => head.push((*i, e)),
            None => rest.push(e),
        }
    }
    head.sort_by_key(|(i, _)| *i);
    ordered.extend(head.into_iter().map(|(_, e)| e));
    ordered.extend(rest);
    debug_assert_eq!(ordered.len(), total);
    reg.emojis = ordered;
    write_registry(&st.data_dir, &reg)?;
    Ok(serde_json::json!({ "count": reg.emojis.len() }))
}

/// 发送自定义表情
#[tauri::command]
pub async fn send_emoji(
    ctx: State<'_, crate::SharedCtx>,
    key: String,
    id: String,
    text: Option<String>,
) -> Result<serde_json::Value, String> {
    send_one(&ctx, &key, &id, text.as_deref().unwrap_or("")).await
}

/// 导出表情包（ids 为空 = 全部）
#[tauri::command]
pub async fn export_emoji_pack(
    ids: Vec<String>,
    dest: String,
    name: Option<String>,
    st: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let pack_name = name.unwrap_or_else(|| "表情包".to_string());
    let n = export_pack(&st.data_dir, &ids, Path::new(&dest), &pack_name)?;
    Ok(serde_json::json!({ "count": n, "path": dest }))
}

/// 预览表情包内容（导入前）
#[tauri::command]
pub async fn inspect_emoji_pack(
    path: String,
    st: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let r = inspect_pack(&st.data_dir, Path::new(&path))?;
    serde_json::to_value(r).map_err(|e| e.to_string())
}

/// 导入表情包（files 为空 = 全部可导入项）
#[tauri::command]
pub async fn import_emoji_pack(
    path: String,
    files: Option<Vec<String>>,
    st: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let sel = files.unwrap_or_default();
    let (n, skipped) = import_pack(&st.data_dir, Path::new(&path), &sel)?;
    Ok(serde_json::json!({
        "imported": n,
        "skipped": skipped.into_iter().map(|(name, reason)| serde_json::json!({ "name": name, "reason": reason })).collect::<Vec<_>>(),
    }))
}

/* ================= 测试 ================= */

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个临时数据目录（与项目其他测试同一套路）
    fn tmp_data(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oim-emoji-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 一张最小的合法 PNG（1x1 透明），用于魔数判定与包往返
    fn png_bytes() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89");
        v.extend_from_slice(b"payload-payload-payload");
        v
    }

    fn gif_bytes() -> Vec<u8> {
        let mut v = b"GIF89a".to_vec();
        v.extend_from_slice(&[0u8; 32]);
        v
    }

    #[test]
    fn every_supported_kind_maps_to_a_stageable_mime() {
        // 五种魔数都要有 MIME，且必须是 net::stage_clipboard_image 认的那几种
        let kinds = ["png", "jpg", "gif", "webp", "bmp"];
        for k in kinds {
            let mime = mime_for_kind(k).unwrap_or_else(|| panic!("{k} 缺 MIME"));
            let dir = tmp_data("mime");
            // 真正落盘一次：能过 stage_clipboard_image 的 MIME 白名单才算对
            let p = crate::net::stage_clipboard_image(&dir, "AAAA", mime)
                .unwrap_or_else(|e| panic!("{mime} 不被 stage_clipboard_image 接受：{e}"));
            assert!(p.is_file(), "{mime} 未落盘");
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with("ipmsgclip_s_"),
                "发送要用官方「粘贴图片」命名，实际：{name}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
        assert_eq!(mime_for_kind("tiff"), None);
        assert_eq!(mime_for_kind(""), None);
    }

    #[test]
    fn detects_image_kind_by_magic_bytes() {
        assert_eq!(detect_image_kind(&png_bytes()), Some("png"));
        assert_eq!(detect_image_kind(&gif_bytes()), Some("gif"));
        assert_eq!(detect_image_kind(&[0xFF, 0xD8, 0xFF, 0xE0, 1, 2]), Some("jpg"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(detect_image_kind(&webp), Some("webp"));
        assert_eq!(detect_image_kind(b"BM\x00\x00"), Some("bmp"));
        // 伪装成 png 的可执行文件/文本一律拒绝
        assert_eq!(detect_image_kind(b"#!/bin/sh\nrm -rf /"), None);
        assert_eq!(detect_image_kind(b""), None);
    }

    #[test]
    fn clean_name_strips_control_chars_and_limits_length() {
        assert_eq!(clean_name("  你好\n世界  "), "你好世界");
        assert_eq!(clean_name("\u{7}"), "表情");
        assert_eq!(clean_name(""), "表情");
        assert_eq!(clean_name(&"字".repeat(100)).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn name_from_path_drops_dirs_and_extension() {
        assert_eq!(name_from_path("/a/b/开心.png"), "开心");
        assert_eq!(name_from_path("C:\\imgs\\cat.GIF"), "cat");
        assert_eq!(name_from_path("noext"), "noext");
        // 点开头的隐藏文件没有扩展名可言，按原名保留（清掉的只是前导点）
        assert_eq!(name_from_path(".hidden"), ".hidden");
    }

    #[test]
    fn rejects_unsafe_pack_entry_names() {
        assert!(safe_entry_name("images/001_a.png"));
        assert!(!safe_entry_name("../evil.png"));
        assert!(!safe_entry_name("images/../../evil.png"));
        assert!(!safe_entry_name("/etc/passwd"));
        assert!(!safe_entry_name("C:/x.png"));
        assert!(!safe_entry_name("images\\x.png"));
        assert!(!safe_entry_name(""));
    }

    #[test]
    fn content_sha_is_length_sensitive_and_stable() {
        let a = content_sha(&png_bytes());
        assert_eq!(a, content_sha(&png_bytes()));
        let mut other = png_bytes();
        other.push(0);
        assert_ne!(a, content_sha(&other));
    }

    #[test]
    fn import_dedupes_and_skips_non_images() {
        let dir = tmp_data("import");
        let sources = vec![
            ("a.png".to_string(), png_bytes()),
            ("b.png".to_string(), png_bytes()), // 同内容 → 去重
            ("bad.png".to_string(), b"not an image".to_vec()),
        ];
        let (ok, skipped) = import_files(&dir, &sources).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(skipped.len(), 2);
        assert!(skipped.iter().any(|(n, _)| n == "a.png" || n == "b.png"));
        // 落盘文件确实存在且内容一致
        let entry = &ok[0];
        assert_eq!(std::fs::read(emoji_dir(&dir).join(&entry.file)).unwrap(), png_bytes());
        // 索引可回读
        let list = load_emojis(&dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "a");
        // 再导入同一张仍然去重
        let (ok2, skipped2) = import_files(&dir, &sources[..1]).unwrap();
        assert!(ok2.is_empty());
        assert_eq!(skipped2.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_enforces_size_limit() {
        let dir = tmp_data("size");
        let mut big = png_bytes();
        big.resize((MAX_EMOJI_BYTES + 1) as usize, 0);
        let (ok, skipped) = import_files(&dir, &[("big.png".to_string(), big)]).unwrap();
        assert!(ok.is_empty());
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].1.contains("MB"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_drops_entries_whose_file_is_gone() {
        let dir = tmp_data("prune");
        let (ok, _) = import_files(&dir, &[("a.png".to_string(), png_bytes())]).unwrap();
        let f = emoji_dir(&dir).join(&ok[0].file);
        std::fs::remove_file(&f).unwrap();
        assert!(load_emojis(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_export_then_inspect_then_import_roundtrips() {
        let dir = tmp_data("pack");
        let (ok, _) = import_files(
            &dir,
            &[("开心.png".to_string(), png_bytes()), ("猫.gif".to_string(), gif_bytes())],
        )
        .unwrap();
        assert_eq!(ok.len(), 2);

        let pack_path = dir.join("out.ipmojis");
        let n = export_pack(&dir, &[], &pack_path, "我的表情包").unwrap();
        assert_eq!(n, 2);
        assert!(pack_path.is_file());

        // 清单是标准 zip：能被 zip 库直接读，且第一个条目是清单
        let f = std::fs::File::open(&pack_path).unwrap();
        let mut za = zip::ZipArchive::new(f).unwrap();
        assert_eq!(za.by_index(0).unwrap().name(), PACK_MANIFEST);
        assert!(za.by_name("README.txt").is_ok());
        drop(za);

        // 在本机（已有这两张）预览：应判为重复，可导入数为 0
        let ins = inspect_pack(&dir, &pack_path).unwrap();
        assert_eq!(ins.format, PACK_FORMAT);
        assert_eq!(ins.name, "我的表情包");
        assert_eq!(ins.total, 2);
        assert_eq!(ins.importable, 0);
        assert!(ins.items.iter().all(|i| i.duplicate && i.kind.is_some()));

        // 清空表情库后再导入整包：两张都回来
        for e in load_emojis(&dir).unwrap() {
            std::fs::remove_file(emoji_dir(&dir).join(&e.file)).unwrap();
        }
        std::fs::write(registry_path(&dir), b"{}").unwrap();
        assert!(load_emojis(&dir).unwrap().is_empty());

        let ins2 = inspect_pack(&dir, &pack_path).unwrap();
        assert_eq!(ins2.importable, 2);
        let (imported, skipped) = import_pack(&dir, &pack_path, &[]).unwrap();
        assert_eq!(imported, 2);
        assert!(skipped.is_empty());
        let list = load_emojis(&dir).unwrap();
        assert_eq!(list.len(), 2);
        let names: HashSet<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains("开心") && names.contains("猫"));
        // 内容一致（gif 那条）
        let gif_entry = list.iter().find(|e| e.name == "猫").unwrap();
        assert_eq!(
            std::fs::read(emoji_dir(&dir).join(&gif_entry.file)).unwrap(),
            gif_bytes()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_import_honours_selection_and_reports_skips() {
        let dir = tmp_data("select");
        import_files(
            &dir,
            &[("a.png".to_string(), png_bytes()), ("b.gif".to_string(), gif_bytes())],
        )
        .unwrap();
        let pack_path = dir.join("out.ipmojis");
        export_pack(&dir, &[], &pack_path, "p").unwrap();
        let ins = inspect_pack(&dir, &pack_path).unwrap();
        let one = ins.items[0].file.clone();
        // 只选一个条目
        let (n, _) = import_pack(&dir, &pack_path, std::slice::from_ref(&one)).unwrap();
        assert_eq!(n, 0, "本机已有同样的图，重复项不应重复入库");
        // 选择不存在的条目 → 静默跳过，不报错
        let (n2, skipped2) = import_pack(&dir, &pack_path, &["images/nope.png".to_string()]).unwrap();
        assert_eq!(n2, 0);
        assert!(skipped2.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_with_unsafe_manifest_is_rejected_per_item() {
        let dir = tmp_data("unsafe");
        std::fs::create_dir_all(emoji_dir(&dir)).unwrap();
        let pack_path = dir.join("evil.ipmojis");
        {
            let f = std::fs::File::create(&pack_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            let manifest = serde_json::json!({
                "format": PACK_FORMAT,
                "version": 1,
                "name": "evil",
                "emojis": [
                    { "name": "x", "file": "../escape.png", "size": 1 },
                    { "name": "y", "file": "/abs.png", "size": 1 },
                    { "name": "ok", "file": "images/ok.png", "size": png_bytes().len() },
                ],
            });
            zw.start_file(PACK_MANIFEST, opts).unwrap();
            zw.write_all(manifest.to_string().as_bytes()).unwrap();
            zw.start_file("images/ok.png", opts).unwrap();
            zw.write_all(&png_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let ins = inspect_pack(&dir, &pack_path).unwrap();
        assert_eq!(ins.total, 3);
        assert_eq!(ins.importable, 1, "只有 images/ok.png 合法");
        let (n, skipped) = import_pack(&dir, &pack_path, &[]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(skipped.len(), 2);
        // 没有任何文件被写到 emojis/ 之外
        assert!(!dir.join("escape.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_with_wrong_format_marker_is_rejected() {
        let dir = tmp_data("format");
        std::fs::create_dir_all(emoji_dir(&dir)).unwrap();
        let pack_path = dir.join("other.ipmojis");
        {
            let f = std::fs::File::create(&pack_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file(PACK_MANIFEST, opts).unwrap();
            zw.write_all(br#"{"format":"something-else","version":1,"emojis":[]}"#)
                .unwrap();
            zw.finish().unwrap();
        }
        let err = inspect_pack(&dir, &pack_path).unwrap_err();
        assert!(err.contains("不是 Open IPMsg"), "实际错误：{err}");
        // 非 zip 文件同样给出可读错误
        let bad = dir.join("bad.ipmojis");
        std::fs::write(&bad, b"plain text").unwrap();
        assert!(inspect_pack(&dir, &bad).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_import_cleans_up_temp_dir() {
        let dir = tmp_data("tmpclean");
        import_files(&dir, &[("a.png".to_string(), png_bytes())]).unwrap();
        let pack_path = dir.join("out.ipmojis");
        export_pack(&dir, &[], &pack_path, "p").unwrap();
        inspect_pack(&dir, &pack_path).unwrap();
        let tmp = emoji_dir(&dir).join(format!("tmp-{}", std::process::id()));
        assert!(!tmp.exists(), "临时解压目录应随 StagedPack 一起删掉");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn src_available_prefers_recorded_path_then_falls_back_to_download_dir() {
        let dir = tmp_data("srcavail");
        // 下载目录：写进 config.json，模拟用户的真实配置
        let dl = dir.join("下载");
        std::fs::create_dir_all(&dl).unwrap();
        std::fs::write(
            dir.join("config.json"),
            format!(r#"{{"download_dir":"{}"}}"#, dl.display()),
        )
        .unwrap();
        let png = dir.join("a.png");
        std::fs::write(&png, png_bytes()).unwrap();
        let size = png_bytes().len() as u64;

        // 1) 记录路径有效 → 直接命中
        let hit = src_available_payload(&png.to_string_lossy(), Some("a.png"), &dir);
        assert_eq!(hit["ok"], serde_json::json!(true));
        assert_eq!(hit["size"], serde_json::json!(size));

        // 2) 记录路径失效（空/不存在）→ 按文件名去下载目录兜底找回
        let moved = dl.join("a.png");
        std::fs::rename(&png, &moved).unwrap();
        let fallback = src_available_payload("", Some("a.png"), &dir);
        assert_eq!(fallback["ok"], serde_json::json!(true), "应能按文件名兜底找到");
        assert_eq!(fallback["size"], serde_json::json!(size));
        assert_eq!(fallback["path"], serde_json::json!(moved.to_string_lossy()));
        let stale = src_available_payload("/nonexistent/x.png", Some("a.png"), &dir);
        assert_eq!(stale["ok"], serde_json::json!(true), "陈旧路径也要能兜底");

        // 3) 没有 config.json → 回落 <data_dir>/接收文件
        let dir2 = tmp_data("srcavail2");
        std::fs::create_dir_all(dir2.join("接收文件")).unwrap();
        std::fs::write(dir2.join("接收文件/b.png"), png_bytes()).unwrap();
        let dflt = src_available_payload("", Some("b.png"), &dir2);
        assert_eq!(dflt["ok"], serde_json::json!(true));

        // 4) 目录不算、路径穿越被挡住、确实没有则 ok=false
        assert_eq!(
            src_available_payload(&dir.to_string_lossy(), None, &dir)["ok"],
            serde_json::json!(false)
        );
        assert_eq!(
            src_available_payload("", Some("../../../etc/passwd"), &dir)["ok"],
            serde_json::json!(false)
        );
        assert_eq!(
            src_available_payload("", Some("nope.png"), &dir)["ok"],
            serde_json::json!(false)
        );
        assert_eq!(src_available_payload("", None, &dir)["ok"], serde_json::json!(false));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// 人工验证辅助：把一张表情导出成真实文件，交给系统解压工具检查结构。
    /// 默认忽略（写的是固定路径），按需运行：
    /// `cargo test --lib write_pack_for_manual_inspection -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn write_pack_for_manual_inspection() {
        let dir = tmp_data("manual");
        import_files(&dir, &[("开心.png".to_string(), png_bytes())]).unwrap();
        let out = std::env::temp_dir().join("oim-emoji-manual.ipmojis");
        let _ = std::fs::remove_file(&out);
        let n = export_pack(&dir, &[], &out, "人工验证包").unwrap();
        println!("exported {n} -> {}", out.display());
        assert!(out.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_requires_at_least_one_emoji() {
        let dir = tmp_data("empty");
        let err = export_pack(&dir, &[], &dir.join("x.ipmojis"), "p").unwrap_err();
        assert!(err.contains("没有可导出的表情"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
