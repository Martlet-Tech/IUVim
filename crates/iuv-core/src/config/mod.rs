//! 引擎配置：集中定义全部配置项（禁止散落在各模块）。
//! 配置来源：默认值 → `%LOCALAPPDATA%\iuv\config.json`（缺省字段自动补默认）。
//! 加载失败（文件缺失/JSON 非法）一律回退默认值，绝不 fail——输入法不得因配置崩溃。
//!
//! 新增配置项的唯一入口：在本模块加字段 + `Default`，序列化自动跟随。
//!
//! P2.1 拆分：枚举/实例状态/IO 分别移入 `enums.rs`/`runtime.rs`/`io.rs`，
//! 本文件保留 `Config` 本体与序列化测试。

pub mod keymap;
pub use keymap::Keymap;

mod enums;
mod io;
mod runtime;

pub use enums::{EngineChoice, InitialMode, Orientation, PunctMode, ScriptMode, ThemeChoice, WidthMode};
pub use io::default_config_path;
pub use runtime::ImeState;

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
    pub initial_state: ImeState,
    /// 按键直通进程名单：命中进程（exe 名，大小写不敏感精确匹配）TSF 层全部按键放行，
    /// 不建会话/无候选窗（输入法在该进程完全透明，游戏场景）。默认空 = 不启用。
    pub passthrough_apps: Vec<String>,
    /// 候选渲染自持进程名单：这些 app 自己绘制候选栏（TSF→IMM 桥消费候选 UI 元素，
    /// 如 WoW 游戏内候选框）→ iuv **抑制自绘候选窗**（避免双候选栏重叠）。
    /// 默认空 = 恒自绘（所有正常 app 都显示 iuv 候选窗）。
    /// 历史教训（2026-08-20）：按 GetTextExt 退化矩形自动判 IMM 应用会误伤真实 TSF
    /// 应用（微信编辑器对折叠 range 返回 2×1 薄光标 → 3 次即误判抑制，候选栏消失），
    /// 故改为显式名单驱动，不再做矩形启发式。
    pub candidate_owner_apps: Vec<String>,
    /// 候选窗主题（light/dark，默认 light；M4 起生效，见 19-m4-cross-render.md）。
    /// 深色切换需重载输入法生效（热切换 M6 设置页做）。
    pub theme: ThemeChoice,
    /// 禁用日志模块列表（denylist；默认空 = 全记录，见 26-log-modules.md）。
    /// Windows 平台 TSF/daemon 消费：log_line 按消息前缀 `[tag]` 匹配，命中即静音。
    /// 引擎本身不记日志；字段仅为共享 config.json 语义（设置页写、两侧读）。
    pub disabled_log_modules: Vec<String>,
    /// 候选生成核心（39-rime-pipeline.md Step3 过渡开关，默认 classic）。
    /// 装载点消费：TSF load_engine / REPL --engine。切换需重载输入法生效。
    pub engine: EngineChoice,
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
            initial_state: ImeState::default(),
            passthrough_apps: Vec::new(),
            candidate_owner_apps: Vec::new(),
            theme: ThemeChoice::Light,
            disabled_log_modules: Vec::new(),
            engine: EngineChoice::Classic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::io::strip_jsonc_comments;
    use super::*;
    use crate::Key;

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
        // 候选渲染自持名单默认空（恒自绘）
        assert!(c.candidate_owner_apps.is_empty());
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
    fn candidate_owner_apps_parse() {
        // 候选渲染自持名单解析 + 缺省空（恒自绘）
        let c: Config = serde_json::from_str(r#"{ "page_size": 5 }"#).unwrap();
        assert!(c.candidate_owner_apps.is_empty());
        let p = tmp_file("cand_owner.json");
        std::fs::write(
            &p,
            r#"{ "candidate_owner_apps": ["Wow.exe"] }"#,
        )
        .unwrap();
        let c2 = Config::from_file(&p);
        assert_eq!(c2.candidate_owner_apps, vec!["Wow.exe".to_owned()]);
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
        assert_eq!(c2.initial_state, ImeState::default());
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