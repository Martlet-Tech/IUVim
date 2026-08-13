//! 引擎配置：集中定义全部配置项（禁止散落在各模块）。
//! 配置来源：默认值 → `%LOCALAPPDATA%\iuv\config.json`（缺省字段自动补默认）。
//! 加载失败（文件缺失/JSON 非法）一律回退默认值，绝不 fail——输入法不得因配置崩溃。
//!
//! 新增配置项的唯一入口：在本模块加字段 + `Default`，序列化自动跟随。

use std::path::PathBuf;

use crate::Key;

pub mod keymap;
pub use keymap::Keymap;

/// 候选窗布局方向。键位语义与布局解耦（由 keymap 配置决定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    /// 竖排：候选一列从上到下
    Vertical,
    /// 横排：候选单行从左到右
    Horizontal,
}

impl Default for Orientation {
    fn default() -> Self {
        Orientation::Vertical
    }
}

/// 引擎配置。
///
/// 默认值：page_size=5, max_candidates=200, max_word_syllables=7,
/// 翻页键 上=PageUp/,/↑、下=PageDown/./↓，候选移动 左=←、右=→，布局竖排。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// 每页候选数（默认 5；建议 ≤9 保证数字键可全选当前页）
    pub page_size: usize,
    /// 全表候选上限
    pub max_candidates: usize,
    /// lattice 词宽上限
    pub max_word_syllables: usize,
    /// 快捷键映射（翻页/候选移动四组语义键）
    pub keymap: Keymap,
    /// 前缀联想：关闭 = 候选仅 exact 匹配（微软化，默认）；开启 = 追加以当前码为前缀的长词（最多 20 条）
    pub candidate_prefix: bool,
    /// 候选窗布局方向（竖排/横排）
    pub candidate_orientation: Orientation,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            page_size: 5,
            max_candidates: 200,
            max_word_syllables: 7,
            keymap: Keymap::default(),
            candidate_prefix: false,
            candidate_orientation: Orientation::Vertical,
        }
    }
}

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
                log_config(&format!("配置加载失败（{e}）：{}，使用默认配置", path.display()));
                return Config::default();
            }
        };
        let text = text.trim_start_matches('\u{FEFF}'); // UTF-8 BOM
        match serde_json::from_str::<Config>(text) {
            Ok(cfg) => {
                log_config(&format!("配置已加载：{}", path.display()));
                cfg
            }
            Err(e) => {
                log_config(&format!("配置解析失败（{e}）：{}，使用默认配置", path.display()));
                Config::default()
            }
        }
    }

    /// 翻页键是否命中（供 REPL/TSF 统一调用）。
    pub fn is_page_key(&self, key: Key) -> Option<Key> {
        self.keymap.page(key)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("iuv-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn default_values() {
        let c = Config::default();
        assert_eq!(c.page_size, 5);
        assert_eq!(c.max_candidates, 200);
        assert_eq!(c.max_word_syllables, 7);
        assert!(!c.candidate_prefix);
        assert!(c.keymap.page_prev.contains(&Key::Char(',')));
        assert!(c.keymap.page_next.contains(&Key::Char('.')));
    }

    #[test]
    fn serde_roundtrip() {
        let c = Config::default();
        let json = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.page_size, c.page_size);
        assert_eq!(back.keymap.page_prev, c.keymap.page_prev);
    }

    #[test]
    fn partial_fields_fill_defaults() {
        // 只写 page_size，其余字段自动补默认（#[serde(default)]）。
        let c: Config = serde_json::from_str(r#"{ "page_size": 9 }"#).unwrap();
        assert_eq!(c.page_size, 9);
        assert_eq!(c.max_candidates, 200);
        assert_eq!(c.keymap.page_next, Keymap::default().page_next);
    }

    #[test]
    fn from_file_missing_falls_back() {
        let c = Config::from_file(&tmp_file("missing.json"));
        assert_eq!(c.page_size, 5); // 回退默认
    }

    #[test]
    fn from_file_invalid_falls_back() {
        let p = tmp_file("bad.json");
        std::fs::write(&p, "{ not json").unwrap();
        let c = Config::from_file(&p);
        assert_eq!(c.page_size, 5);
    }

    #[test]
    fn from_file_valid_overrides() {
        let p = tmp_file("good.json");
        std::fs::write(
            &p,
            r#"{ "page_size": 7, "keymap": { "page_prev": ["["] } }"#,
        )
        .unwrap();
        let c = Config::from_file(&p);
        assert_eq!(c.page_size, 7);
        assert!(c.keymap.page_prev.contains(&Key::Char('[')));
        // 未写的 page_next 用默认
        assert!(c.keymap.page_next.contains(&Key::Char('.')));
    }

    #[test]
    fn from_file_bom_tolerated() {
        let p = tmp_file("bom.json");
        // 模拟记事本/PS 5.1 保存：UTF-8 BOM + JSON
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(r#"{ "page_size": 8 }"#.as_bytes());
        std::fs::write(&p, &bytes).unwrap();
        let c = Config::from_file(&p);
        assert_eq!(c.page_size, 8);
    }

    #[test]
    fn is_page_key_matches() {
        let c = Config::default();
        assert_eq!(c.is_page_key(Key::Char(',')), Some(Key::PageUp));
        assert_eq!(c.is_page_key(Key::Char('.')), Some(Key::PageDown));
        assert_eq!(c.is_page_key(Key::Char('a')), None);
    }
}
