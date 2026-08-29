//! 配置加载/迁移（P2.1 从 config/mod.rs 拆出）：config.json 读路径 + 旧键迁移 shim。

use crate::config::Config;

impl Config {
    /// 加载配置：`%LOCALAPPDATA%\iuv\config.json`。
    /// 文件缺失 / 解析失败 / 部分字段缺失 → 用默认值补齐，绝不 fail。
    pub fn load() -> Config {
        let Some(path) = default_config_path() else {
            return Config::default();
        };
        Self::from_file(&path)
    }

    /// 从指定 JSON 文件加载；失败回退默认。
    /// 兼容 UTF-8 BOM（记事本/PS 5.1 保存常见），读取后剥除。
    pub fn from_file(path: &std::path::Path) -> Config {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Config::default(),
        };
        let text = strip_bom(&text);
        let text = strip_jsonc_comments(&text); // 兼容带 // 注释的配置（安装器产出的默认文件）
        let v = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => migrate_initial_state(migrate_keymap(v)),
            Err(_) => return Config::default(),
        };
        serde_json::from_value::<Config>(v).unwrap_or_default()
    }
}

/// 旧配置迁移 shim（2026-08-19，见 `docs/plan/28-initial-state-settings.md`）：
/// 旧顶层 `english_punctuation: bool` → 新 `initial_state.punct` 枚举（bool→"english"/"chinese"）。
/// 新配置已含 `initial_state` 节点时不动（新节点优先）；缺旧键则纯默认（serde 补）。
fn migrate_initial_state(mut v: serde_json::Value) -> serde_json::Value {
    if v.get("initial_state").is_some() {
        return v;
    }
    let Some(obj) = v.as_object_mut() else { return v };
    let Some(punct) = obj
        .get("english_punctuation")
        .and_then(|x| x.as_bool())
        .map(|b| if b { "english" } else { "chinese" })
    else {
        return v;
    };
    obj.insert("initial_state".into(), serde_json::json!({ "punct": punct }));
    v
}

/// keymap 旧格式迁移 shim（2026-08-27，41-keymap-settings.md §3）：
/// 旧 `"keymap": {"page_prev": ["PageUp", ",", "Up"], ...}`（每项 = Vec<Key> 字符串数组）
/// → 新两槽格式 `{"page_prev": {"primary": "PageUp", "secondary": ","}, ...}`。
/// 只迁移两槽语义字段（会话 7 + 全局 6）；取数组前两键作 primary/secondary，第三键起丢弃
/// （两槽模型容量上限）。新格式对象节点（含 primary/secondary）原样保留。
const KEYMAP_SESSION_FIELDS: &[&str] = &[
    "page_prev",
    "page_next",
    "candidate_prev",
    "candidate_next",
    "swap_left",
    "swap_right",
    "hide_candidate",
];
const KEYMAP_GLOBAL_FIELDS: &[&str] = &[
    "toggle_mode",
    "toggle_width",
    "toggle_script",
    "toggle_punct",
    "open_settings",
    "toggle_toolbar",
];

pub fn migrate_keymap(mut v: serde_json::Value) -> serde_json::Value {
    let Some(km) = v.get_mut("keymap").and_then(|x| x.as_object_mut()) else {
        return v;
    };
    for field in KEYMAP_SESSION_FIELDS.iter().chain(KEYMAP_GLOBAL_FIELDS) {
        let Some(arr) = km.get(*field).and_then(|x| x.as_array()) else {
            continue; // 缺字段 / 已是对象（新格式）→ 跳过
        };
        let mut names: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        names.truncate(2);
        let obj = serde_json::json!({
            "primary": names.first(),
            "secondary": names.get(1),
        });
        km.insert((*field).into(), obj);
    }
    v
}

/// 默认配置路径：`<iuv 数据目录>\config.json`（与词库同目录，见 [`crate::paths`]）。
/// 目录链无法解析时返回 None（用默认配置）。
pub fn default_config_path() -> Option<std::path::PathBuf> {
    Some(crate::paths::iuv_dir()?.join("config.json"))
}

/// 剥 UTF-8 BOM（记事本/PS 5.1 保存常见）。
pub fn strip_bom(text: &str) -> &str {
    text.trim_start_matches('\u{FEFF}')
}

/// 剥 JSONC 行注释（`//` 到行尾）：字符串内不剥（含 `\"` 转义），行尾 CR 保留。
/// serde_json 不支持注释，安装器产出的带注释默认配置经此预处理后解析。
pub fn strip_jsonc_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_str = false;
    let mut prev_escape = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            out.push(c as char);
            if prev_escape {
                prev_escape = false;
            } else if c == b'\\' {
                prev_escape = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
        } else if c == b'"' {
            in_str = true;
            out.push(c as char);
            i += 1;
        } else if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // 跳到行尾（保留换行符，行号不漂移）
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}