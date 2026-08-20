//! 配置加载/迁移（P2.1 从 config/mod.rs 拆出）：config.json 读路径 + 旧键迁移 shim。

use crate::config::Config;
use std::path::PathBuf;

impl Config {
    /// 加载配置：`%LOCALAPPDATA%\iuv\config.json`。
    /// 文件缺失 / 解析失败 / 部分字段缺失 → 用默认值补齐，绝不 fail。
    pub fn load() -> Config {
        let Some(path) = default_config_path() else {
            return Config::default();
        };
        Self::from_file(&path)
    }

    /// 从指定 JSON 文件加载；失败回退默认。供测试与 REPL `--config` 用。
    /// 兼容 UTF-8 BOM（记事本/PS 5.1 保存常见），读取后剥除。
    pub fn from_file(path: &std::path::Path) -> Config {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                log_config(&format!(
                    "配置加载失败（{e}）：{}，使用默认配置",
                    path.display()
                ));
                return Config::default();
            }
        };
        let text = text.trim_start_matches('\u{FEFF}'); // UTF-8 BOM
        let text = strip_jsonc_comments(text); // 兼容带 // 注释的配置（安装器产出的默认文件）
        let v = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => migrate_initial_state(v),
            Err(e) => {
                log_config(&format!(
                    "配置解析失败（{e}）：{}，使用默认配置",
                    path.display()
                ));
                return Config::default();
            }
        };
        match serde_json::from_value::<Config>(v) {
            Ok(cfg) => {
                log_config(&format!("配置已加载：{}", path.display()));
                cfg
            }
            Err(e) => {
                log_config(&format!(
                    "配置解析失败（{e}）：{}，使用默认配置",
                    path.display()
                ));
                Config::default()
            }
        }
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

/// 默认配置路径：%LOCALAPPDATA%\iuv\config.json（与词库同目录）。
/// 跨平台：非 Windows 或无 LOCALAPPDATA 时返回 None（用默认配置）。
pub fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .or_else(|| std::env::var("APPDATA").ok().map(|a| format!("{a}\\Local")))
        .or_else(|| std::env::var("HOME").ok())?;
    Some(PathBuf::from(base).join("iuv").join("config.json"))
}

/// 配置日志：iuv-core 无日志设施，仅在失败时静默（输入法场景由 TSF 层日志覆盖）。
fn log_config(_msg: &str) {}

/// 剥 JSONC 行注释（`//` 到行尾）：字符串内不剥（含 `\"` 转义），行尾 CR 保留。
/// serde_json 不支持注释，安装器产出的带注释默认配置经此预处理后解析。
pub(crate) fn strip_jsonc_comments(text: &str) -> String {
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