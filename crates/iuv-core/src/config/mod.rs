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

/// 候选窗/菜单主题（M4 起，见 `docs/plan/19-m4-cross-render.md`）。
/// 呈现层（iuv-tsf candwin.rs）装配时映射到 iuv-ui 的 `theme_light()`/`theme_dark()`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    /// 浅色（默认）：白底近黑字（对齐原 GDI 观感）
    Light,
    /// 深色：0x202020 系底 + 浅色字
    Dark,
}

impl Default for ThemeChoice {
    fn default() -> Self {
        ThemeChoice::Light
    }
}

/// 新 TSF 实例初始模式（中/英）。`initial_state.mode` 驱动 Activate 时 OPENCLOSE 初值
/// （见 `docs/plan/28-initial-state-settings.md`）。中文默认 = 激活即打开（MS IME 同款语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InitialMode {
    /// 中文（默认）：激活后输入法为中文模式
    Chinese,
    /// 英文：每个新 TSF 实例从英文模式起（Ctrl+Space 可切回中文）
    English,
}

impl Default for InitialMode {
    fn default() -> Self {
        InitialMode::Chinese
    }
}

/// 新 TSF 实例初始宽度（半角/全角）。半角默认；全角行为后置（仅存默认值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WidthMode {
    /// 半角（默认）
    Half,
    /// 全角（仅存默认值，行为后置）
    Full,
}

impl Default for WidthMode {
    fn default() -> Self {
        WidthMode::Half
    }
}

/// 新 TSF 实例初始字形（简体/繁体）。简体默认；繁体行为后置（仅存默认值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptMode {
    /// 简体（默认）
    Simplified,
    /// 繁体（仅存默认值，行为后置）
    Traditional,
}

impl Default for ScriptMode {
    fn default() -> Self {
        ScriptMode::Simplified
    }
}

/// 中文状态标点风格（中文标点/英文标点）。替代旧顶层 `english_punctuation: bool`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PunctMode {
    /// 中文标点（默认，全角：`，`/`。`；主流输入法默认）
    Chinese,
    /// 英文标点（中文状态按标点键直通英文形：`，`→`,`）
    English,
}

impl Default for PunctMode {
    fn default() -> Self {
        PunctMode::Chinese
    }
}

/// 新 TSF 实例初始状态（`initial_state` 配置节点，见 `docs/plan/28-initial-state-settings.md`）。
/// 中/英激活强制设默认；半角/全角、简体/繁体仅存默认值（行为后置）。
/// 默认 = 主流：中文/半角/简体/中文标点（与旧版零行为变化）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InitialState {
    /// 模式：中文（默认）/ 英文
    pub mode: InitialMode,
    /// 宽度：半角（默认）/ 全角
    pub width: WidthMode,
    /// 字形：简体（默认）/ 繁体
    pub script: ScriptMode,
    /// 标点：中文标点（默认）/ 英文标点
    pub punct: PunctMode,
}

impl Default for InitialState {
    fn default() -> Self {
        InitialState {
            mode: InitialMode::Chinese,
            width: WidthMode::Half,
            script: ScriptMode::Simplified,
            punct: PunctMode::Chinese,
        }
    }
}

/// 引擎配置。
///
/// 默认值：page_size=5, max_candidates=1024, max_word_syllables=7,
/// 翻页键 上=PageUp/,/↑、下=PageDown/./↓，候选移动 左=←、右=→，布局竖排。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// 每页候选数（默认 5；建议 ≤9 保证数字键可全选当前页）
    pub page_size: usize,
    /// 全表候选上限（默认 1024：单字全量可达——wei 450/sh 978 同音字全给翻页可达，
    /// 微软对齐；极端多段输入的总量预算，可配小值限制）
    pub max_candidates: usize,
    /// lattice 词宽上限
    pub max_word_syllables: usize,
    /// 快捷键映射（翻页/候选移动四组语义键）
    pub keymap: Keymap,
    /// 前缀联想：关闭 = 候选仅 exact 匹配（微软化，默认）；开启 = 追加以当前码为前缀的长词（最多 20 条）
    pub candidate_prefix: bool,
    /// 候选窗布局方向（竖排/横排）
    pub candidate_orientation: Orientation,
    /// 新 TSF 实例初始状态（中/英、半/全角、简/繁、标点风格）。默认主流值；
    /// 旧版顶层 `english_punctuation: bool` 经 from_file 迁移 shim 并入 `initial_state.punct`。
    pub initial_state: InitialState,
    /// 按键直通进程名单：命中进程（exe 名，大小写不敏感精确匹配）TSF 层全部按键放行，
    /// 不建会话/无候选窗（输入法在该进程完全透明，游戏场景）。默认空 = 不启用。
    pub passthrough_apps: Vec<String>,
    /// 候选窗主题（light/dark，默认 light；M4 起生效，见 19-m4-cross-render.md）。
    /// 深色切换需重载输入法生效（热切换 M6 设置页做）。
    pub theme: ThemeChoice,
    /// 禁用日志模块列表（denylist；默认空 = 全记录，见 26-log-modules.md）。
    /// Windows 平台 TSF/daemon 消费：log_line 按消息前缀 `[tag]` 匹配，命中即静音。
    /// 引擎本身不记日志；字段仅为共享 config.json 语义（设置页写、两侧读）。
    pub disabled_log_modules: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            page_size: 5,
            max_candidates: 1024,
            max_word_syllables: 7,
            keymap: Keymap::default(),
            candidate_prefix: false,
            candidate_orientation: Orientation::Vertical,
            initial_state: InitialState::default(),
            passthrough_apps: Vec::new(),
            theme: ThemeChoice::Light,
            disabled_log_modules: Vec::new(),
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

    /// 翻页键是否命中（供 REPL/TSF 统一调用）。
    pub fn is_page_key(&self, key: Key) -> Option<Key> {
        self.keymap.page(key)
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

    fn tmp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("iuv-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn default_values() {
        let c = Config::default();
        assert_eq!(c.page_size, 5);
        assert_eq!(c.max_candidates, 1024);
        assert_eq!(c.max_word_syllables, 7);
        assert!(!c.candidate_prefix);
        assert!(c.keymap.page_prev.contains(&Key::Char(',')));
        assert!(c.keymap.page_next.contains(&Key::Char('.')));
        // 直通名单默认空（不启用）
        assert!(c.passthrough_apps.is_empty());
        // 主题默认浅色
        assert_eq!(c.theme, ThemeChoice::Light);
        // 日志禁用模块默认空（全记录）
        assert!(c.disabled_log_modules.is_empty());
    }

    #[test]
    fn theme_deserialize() {
        // 显式 dark / 缺字段（#[serde(default)]）→ 默认 light。
        let c: Config = serde_json::from_str(r#"{ "theme": "dark" }"#).unwrap();
        assert_eq!(c.theme, ThemeChoice::Dark);
        let c2: Config = serde_json::from_str(r#"{ "page_size": 5 }"#).unwrap();
        assert_eq!(c2.theme, ThemeChoice::Light, "缺字段补默认 light");
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
        assert_eq!(c.max_candidates, 1024);
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
    fn passthrough_apps_parse() {
        // 白名单解析：精确进程名列表
        let p = tmp_file("passthrough.json");
        std::fs::write(
            &p,
            r#"{ "passthrough_apps": ["cyberpunk2077.exe", "dota2.exe"] }"#,
        )
        .unwrap();
        let c = Config::from_file(&p);
        assert_eq!(
            c.passthrough_apps,
            vec!["cyberpunk2077.exe".to_owned(), "dota2.exe".to_owned()]
        );
    }

    #[test]
    fn disabled_log_modules_parse() {
        // 缺省字段 → 空（全记录）
        let c: Config = serde_json::from_str(r#"{ "page_size": 5 }"#).unwrap();
        assert!(c.disabled_log_modules.is_empty());
        // 显式列表解析
        let c2: Config =
            serde_json::from_str(r#"{ "disabled_log_modules": ["uielem", "key"] }"#).unwrap();
        assert_eq!(c2.disabled_log_modules, vec!["uielem".to_owned(), "key".to_owned()]);
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
    fn from_file_jsonc_comments() {
        // 安装器产出的带 // 注释默认配置：可解析，注释剥除后字段生效。
        let p = tmp_file("commented.json");
        std::fs::write(
            &p,
            "{\n\
             // 每页候选数\n\
             \"page_size\": 9, // 行尾注释\n\
             \"candidate_orientation\": \"horizontal\" // 横排\n\
             }",
        )
        .unwrap();
        let c = Config::from_file(&p);
        assert_eq!(c.page_size, 9);
        assert_eq!(c.candidate_orientation, Orientation::Horizontal);
    }

    #[test]
    fn strip_comments_keeps_strings() {
        // 字符串值内的 //（如翻页键"//"自定义？）不被误剥；转义引号不破字符串态。
        let src = r#"{"a": "http://x", "b": "//y"}"#;
        assert_eq!(
            strip_jsonc_comments(src),
            r#"{"a": "http://x", "b": "//y"}"#
        );
        let src2 = "{\"a\": 1 // 注释\n}";
        assert_eq!(strip_jsonc_comments(src2), "{\"a\": 1 \n}");
    }

    #[test]
    fn is_page_key_matches() {
        let c = Config::default();
        assert_eq!(c.is_page_key(Key::Char(',')), Some(Key::PageUp));
        assert_eq!(c.is_page_key(Key::Char('.')), Some(Key::PageDown));
        assert_eq!(c.is_page_key(Key::Char('a')), None);
    }

    #[test]
    fn theme_defaults_light() {
        let c = Config::default();
        assert_eq!(c.theme, ThemeChoice::Light);
        // serde 序列化：light
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"theme\":\"light\""));
    }

    #[test]
    fn theme_deserialize_dark() {
        let c: Config = serde_json::from_str(r#"{ "theme": "dark" }"#).unwrap();
        assert_eq!(c.theme, ThemeChoice::Dark);
    }

    #[test]
    fn theme_unknown_value_falls_back_default() {
        // 未知枚举值：serde 拒绝 → from_file 整体回退默认（theme = Light）。
        let p = tmp_file("theme_bad.json");
        std::fs::write(&p, r#"{ "theme": "rainbow" }"#).unwrap();
        let c = Config::from_file(&p);
        assert_eq!(c.theme, ThemeChoice::Light);
        // 缺字段（#[serde(default)]）→ 默认 Light
        let c2: Config = serde_json::from_str(r#"{ "page_size": 5 }"#).unwrap();
        assert_eq!(c2.theme, ThemeChoice::Light);
    }

    #[test]
    fn theme_roundtrip() {
        let c: Config = serde_json::from_str(r#"{ "theme": "dark" }"#).unwrap();
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"theme\":\"dark\""));
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.theme, ThemeChoice::Dark);
    }

    #[test]
    fn initial_state_defaults() {
        // 初始状态默认 = 主流（中文/半角/简体/中文标点），与旧版零行为变化。
        let c = Config::default();
        assert_eq!(c.initial_state.mode, InitialMode::Chinese, "默认中文");
        assert_eq!(c.initial_state.width, WidthMode::Half, "默认半角");
        assert_eq!(c.initial_state.script, ScriptMode::Simplified, "默认简体");
        assert_eq!(c.initial_state.punct, PunctMode::Chinese, "默认中文标点");
        // 缺 initial_state 节点（旧配置）→ serde 补全默认
        let c2: Config = serde_json::from_str(r#"{ "page_size": 5 }"#).unwrap();
        assert_eq!(c2.initial_state, InitialState::default());
        // 显式配置可序列化/反序列化
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"initial_state\":"));
        let c3: Config = serde_json::from_str(
            r#"{ "initial_state": { "mode": "english", "punct": "english" } }"#,
        )
        .unwrap();
        assert_eq!(c3.initial_state.mode, InitialMode::English);
        assert_eq!(c3.initial_state.punct, PunctMode::English);
        assert_eq!(c3.initial_state.width, WidthMode::Half, "未写字段补默认");
    }

    #[test]
    fn migrate_legacy_english_punctuation() {
        // 旧顶层 english_punctuation: bool → initial_state.punct 枚举（升级不丢设置）。
        let p = tmp_file("legacy_ep_true.json");
        std::fs::write(&p, r#"{ "english_punctuation": true }"#).unwrap();
        let c = Config::from_file(&p);
        assert_eq!(c.initial_state.punct, PunctMode::English, "true → 英文标点");
        assert_eq!(c.initial_state.mode, InitialMode::Chinese, "其余字段默认");
        let p2 = tmp_file("legacy_ep_false.json");
        std::fs::write(&p2, r#"{ "english_punctuation": false }"#).unwrap();
        let c2 = Config::from_file(&p2);
        assert_eq!(c2.initial_state.punct, PunctMode::Chinese);
        // 新节点优先：残留旧键时不再迁移
        let p3 = tmp_file("legacy_ep_both.json");
        std::fs::write(
            &p3,
            r#"{ "english_punctuation": true, "initial_state": { "punct": "chinese" } }"#,
        )
        .unwrap();
        let c3 = Config::from_file(&p3);
        assert_eq!(c3.initial_state.punct, PunctMode::Chinese, "新节点优先");
    }
}
