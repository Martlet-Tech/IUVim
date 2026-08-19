//! daemon 设置项读写：`%LOCALAPPDATA%\iuv\config.json`（与 iuv-core `Config` 同目录同字段格式）。
//!
//! 写 = 读现有 JSON → **补丁式更新已知字段**（保留未知字段：keymap/page_size 等）→ 原子写回
//! （tmp + 先删后 rename）。与 iuv-core 的容忍读兼容：剥 JSONC `//` 注释 + UTF-8 BOM。
//! 字段格式与 iuv-core 对齐：`"theme": "light"|"dark"`、`"passthrough_apps": ["a.exe", ...]`。

use std::io;
use std::path::PathBuf;

/// daemon 可管理的设置项（iuv-core Config 的子集；其余字段读改写时保留）。
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    /// 候选窗/菜单主题："light" | "dark"。
    pub theme: String,
    /// 候选窗布局方向："vertical" | "horizontal"（竖排/横排，iuv-core `candidate_orientation`）。
    pub candidate_orientation: String,
    /// 按键直通进程名列表（exe 名，大小写不敏感精确匹配，TSF 层消费）。
    pub passthrough_apps: Vec<String>,
    /// 禁用日志模块列表（denylist；默认空 = 全记录，见 26-log-modules.md）。
    /// 设置页开发者标签勾选、TSF/daemon 两侧 log_line 按消息 `[tag]` 过滤。
    pub disabled_log_modules: Vec<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            theme: "light".into(),
            candidate_orientation: "vertical".into(),
            passthrough_apps: Vec::new(),
            disabled_log_modules: Vec::new(),
        }
    }
}

/// 配置路径：%LOCALAPPDATA%\iuv\config.json（跨平台：无 LOCALAPPDATA 时返回 None）。
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .or_else(|| std::env::var("APPDATA").ok().map(|a| format!("{a}\\Local")))
        .or_else(|| std::env::var("HOME").ok())?;
    Some(PathBuf::from(base).join("iuv").join("config.json"))
}

/// 读取配置（容忍缺失/坏 JSON/未知值 → 默认）。绝不失败。
pub fn load_config() -> DaemonConfig {
    let Some(path) = config_path() else {
        return DaemonConfig::default();
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return DaemonConfig::default(),
    };
    let text = text.trim_start_matches('\u{FEFF}'); // UTF-8 BOM
    let text = strip_jsonc_comments(text); // 兼容安装器产出的带注释配置
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return DaemonConfig::default();
    };
    let mut cfg = DaemonConfig::default();
    if let Some(t) = v.get("theme").and_then(|x| x.as_str()) {
        if t == "dark" || t == "light" {
            cfg.theme = t.to_string();
        }
    }
    if let Some(o) = v.get("candidate_orientation").and_then(|x| x.as_str()) {
        if o == "vertical" || o == "horizontal" {
            cfg.candidate_orientation = o.to_string();
        }
    }
    if let Some(arr) = v.get("passthrough_apps").and_then(|x| x.as_array()) {
        cfg.passthrough_apps = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
    }
    if let Some(arr) = v.get("disabled_log_modules").and_then(|x| x.as_array()) {
        cfg.disabled_log_modules = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
    }
    cfg
}

/// 保存设置：读现有 JSON → 补丁 theme/candidate_orientation/passthrough_apps/disabled_log_modules
/// （保留未知字段）→ 原子写回。
pub fn save_config(
    theme: &str,
    candidate_orientation: &str,
    passthrough_apps: &[String],
    disabled_log_modules: &[String],
) -> io::Result<()> {
    let Some(path) = config_path() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "无 LOCALAPPDATA，无法写配置",
        ));
    };
    // 读现有（剥 BOM/注释；失败 → 空对象）。保留未知字段：keymap/page_size 等。
    let mut root: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .map(|t| {
            let t = t.trim_start_matches('\u{FEFF}');
            strip_jsonc_comments(t)
        })
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if let Some(obj) = root.as_object_mut() {
        obj.insert("theme".into(), serde_json::Value::String(theme.into()));
        obj.insert(
            "candidate_orientation".into(),
            serde_json::Value::String(candidate_orientation.into()),
        );
        obj.insert(
            "passthrough_apps".into(),
            serde_json::Value::Array(
                passthrough_apps
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
        obj.insert(
            "disabled_log_modules".into(),
            serde_json::Value::Array(
                disabled_log_modules
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
    } else {
        // 现有文件顶层非对象（异常）→ 重建。
        root = serde_json::json!({
            "theme": theme,
            "candidate_orientation": candidate_orientation,
            "passthrough_apps": passthrough_apps,
            "disabled_log_modules": disabled_log_modules,
        });
    }
    let text = serde_json::to_string_pretty(&root)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "配置目录不存在"))?;
    std::fs::create_dir_all(dir)?;
    // 原子写：tmp + 先删后 rename（Windows rename 不覆盖已存在文件）。
    let tmp = dir.join("config.json.tmp");
    std::fs::write(&tmp, text)?;
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&tmp, &path)
}

/// 剥 JSONC 行注释（`//` 到行尾）：字符串内不剥（含 `\"` 转义），行尾 CR 保留。
/// 与 iuv-core `Config` 读取逻辑同款（serde_json 不支持注释）。
fn strip_jsonc_comments(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 两测试都改 LOCALAPPDATA 环境变量（进程级全局），必须串行。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn strip_comments_keeps_strings() {
        let src = r#"{"a": "http://x", "theme": "dark"}"#;
        assert_eq!(
            strip_jsonc_comments(src),
            r#"{"a": "http://x", "theme": "dark"}"#
        );
        let src2 = "{\"a\": 1 // 注释\n}";
        assert_eq!(strip_jsonc_comments(src2), "{\"a\": 1 \n}");
    }

    #[test]
    fn save_preserves_unknown_fields() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("iuv-daemon-config-test-{}", std::process::id()));
        // config_path() = %LOCALAPPDATA%\iuv\config.json → 测试目录下再建 iuv 子目录。
        let iuv_dir = dir.join("iuv");
        std::fs::create_dir_all(&iuv_dir).unwrap();
        let path = iuv_dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"page_size": 7, "keymap": {"page_prev": ["["]}, "theme": "light"}"#,
        )
        .unwrap();
        // 用环境变量临时替换 LOCALAPPDATA 指向测试目录
        std::env::set_var("LOCALAPPDATA", &dir);
        save_config(
            "dark",
            "horizontal",
            &["notepad.exe".to_string()],
            &["uielem".to_string()],
        )
        .unwrap();
        // 未知字段保留
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["page_size"], 7);
        assert_eq!(v["keymap"]["page_prev"][0], "[");
        // 已知字段更新
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["candidate_orientation"], "horizontal");
        assert_eq!(v["passthrough_apps"][0], "notepad.exe");
        assert_eq!(v["disabled_log_modules"][0], "uielem");
        // 重新加载
        let cfg = load_config();
        assert_eq!(cfg.theme, "dark");
        assert_eq!(cfg.candidate_orientation, "horizontal");
        assert_eq!(cfg.passthrough_apps, vec!["notepad.exe".to_string()]);
        assert_eq!(cfg.disabled_log_modules, vec!["uielem".to_string()]);
        let _ = std::env::remove_var("LOCALAPPDATA");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_uses_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("iuv-daemon-config-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LOCALAPPDATA", &dir);
        let cfg = load_config();
        assert_eq!(cfg.theme, "light");
        assert_eq!(cfg.candidate_orientation, "vertical", "缺省布局竖排");
        assert!(cfg.passthrough_apps.is_empty());
        assert!(cfg.disabled_log_modules.is_empty(), "缺省字段默认全记录");
        let _ = std::env::remove_var("LOCALAPPDATA");
        let _ = std::fs::remove_dir_all(&dir);
    }
}