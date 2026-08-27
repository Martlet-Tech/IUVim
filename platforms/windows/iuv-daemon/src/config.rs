//! daemon 设置项读写：`%LOCALAPPDATA%\iuv\config.json`（与 iuv-core `Config` 同目录同字段格式）。
//!
//! 写 = 读现有 JSON → **补丁式更新已知字段**（保留未知字段：keymap/max_candidates 等）→ 原子写回
//! （tmp + 先删后 rename）。与 iuv-core 的容忍读兼容：剥 JSONC `//` 注释 + UTF-8 BOM。
//! 字段格式与 iuv-core 对齐：`"theme": "light"|"dark"`、`"initial_state": {...}`、
//! `"passthrough_apps": ["a.exe", ...]`、`"candidate_owner_apps": ["wow.exe", ...]`。

/// 按键直通默认名单：近五年卖爆级 **3A 单机大作**（全程无中文输入需求，整进程隐身
/// 换零按键干扰——Raw Input 与 TSF 双通道并行接收同一按键的游戏，见
/// knowledge/tsf-interaction.md 应用责任小节）。设置页「恢复默认」按钮的回填源。
/// 同步责任：install.ps1 / dev-deploy.ps1 的 config 模板数组须与此保持一致。
pub const DEFAULT_PASSTHROUGH_APPS: &[&str] = &[
    "Cyberpunk2077.exe",         // 赛博朋克2077
    "b1-Win64-Shipping.exe",     // 黑神话：悟空（游戏本体）
    "b1.exe",                    // 黑神话：悟空（启动器）
    "eldenring.exe",             // 艾尔登法环
    "bg3.exe",                   // 博德之门3
    "RDR2.exe",                  // 荒野大镖客2
    "MonsterHunterWilds.exe",    // 怪物猎人：荒野
    "Starfield.exe",             // 星空
];

/// 候选自绘抑制默认名单：预置**要在游戏内打中文**的知名游戏（它们自绘候选栏，
/// iuv 需抑制自绘避免双框；数据经 ITfCandidateListUIElement 供游戏拉取）。
/// 与 passthrough 的分工：要打中文 → 本名单（保留输入）；纯单机 → passthrough。
/// 实测依据：wow pbshow=true 但桥转数据（2026-08-16/22 两轮）、暗黑4 pbshow=false
/// 自带候选框（knowledge/tsf-interaction.md）。同步责任同上。
pub const DEFAULT_CANDIDATE_OWNER_APPS: &[&str] = &[
    "wow.exe",                  // 魔兽世界
    "WowClassic.exe",           // 魔兽怀旧服
    "Diablo IV.exe",            // 暗黑破坏神4
    "Diablo III64.exe",         // 暗黑破坏神3
    "League of Legends.exe",    // 英雄联盟（游戏内进程）
    "TslGame.exe",              // 绝地求生
    "Gw2-64.exe",               // 激战2
    "JX3ClientX64.exe",         // 剑网3重制版
    "JX3Client.exe",            // 剑网3老客户端
    "crossfire.exe",            // 穿越火线经典区
];

use std::io;
use std::path::PathBuf;

/// daemon 可管理的设置项（iuv-core Config 的子集；其余字段读改写时保留）。
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    /// 候选窗/菜单主题："light" | "dark"。
    pub theme: String,
    /// 候选窗布局方向："vertical" | "horizontal"（竖排/横排，iuv-core `candidate_orientation`）。
    pub candidate_orientation: String,
    /// 每页候选数（默认 5；建议 ≤9 保证数字键可全选当前页；越界钳回 5..=9）。
    pub page_size: usize,
    /// 新 TSF 实例初始状态（中/英、半/全角、简/繁、标点风格；复用 iuv-core 类型，单一事实源）。
    /// 旧顶层 `english_punctuation: bool` 键在 load 时迁移并入、save 时清理（见 28-initial-state-settings.md）。
    pub initial_state: iuv_core::ImeState,
    /// 按键直通进程名列表（exe 名，大小写不敏感精确匹配，TSF 层消费）。
    pub passthrough_apps: Vec<String>,
    /// 候选渲染自持进程名列表（exe 名，大小写不敏感精确匹配；命中进程 iuv 抑制自绘候选窗，
    /// 由应用自己绘制候选栏，如 WoW 游戏内候选框）。默认空 = 恒自绘。
    pub candidate_owner_apps: Vec<String>,
    /// 禁用日志模块列表（denylist；默认空 = 全记录，见 26-log-modules.md）。
    /// 设置页开发者标签勾选、TSF/daemon 两侧 log_line 按消息 `[tag]` 过滤。
    pub disabled_log_modules: Vec<String>,
    /// 快捷键映射（41-keymap-settings.md §3）：会话内 7 组 + 全局热键 6 组，各主/备两槽。
    /// 会话组 TSF 消费（config_epoch 热载已通）；全局组 daemon `RegisterHotKey` 消费。
    pub keymap: iuv_core::Keymap,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            theme: "light".into(),
            candidate_orientation: "vertical".into(),
            page_size: 5,
            initial_state: iuv_core::ImeState::default(),
            passthrough_apps: Vec::new(),
            candidate_owner_apps: Vec::new(),
            disabled_log_modules: Vec::new(),
            keymap: iuv_core::Keymap::default(),
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
    if let Some(n) = v.get("page_size").and_then(|x| x.as_u64()) {
        cfg.page_size = n.clamp(5, 9) as usize;
    }
    if let Some(node) = v.get("initial_state") {
        cfg.initial_state = parse_initial_state(node);
    } else if let Some(b) = v.get("english_punctuation").and_then(|x| x.as_bool()) {
        // 旧顶层 english_punctuation 迁移兜底（新节点缺失时读旧键，见 28-initial-state-settings.md）
        cfg.initial_state.punct = if b {
            iuv_core::PunctMode::English
        } else {
            iuv_core::PunctMode::Chinese
        };
    }
    if let Some(arr) = v.get("passthrough_apps").and_then(|x| x.as_array()) {
        cfg.passthrough_apps = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
    }
    if let Some(arr) = v.get("candidate_owner_apps").and_then(|x| x.as_array()) {
        cfg.candidate_owner_apps = arr
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
    // keymap 读取：直接反序列化（serde 两槽对象；旧数组格式在 daemon 侧不兼容——
    // TSF 侧 from_file 已迁移落盘，daemon 读的配置文件始终为新格式；异常 → 默认）。
    if let Some(node) = v.get("keymap") {
        cfg.keymap = serde_json::from_value(node.clone()).unwrap_or_default();
    }
    cfg
}

/// 解析 `initial_state` 节点（复用 iuv-core 类型与 serde 规则：缺字段补默认、未知值回退默认）。
fn parse_initial_state(node: &serde_json::Value) -> iuv_core::ImeState {
    serde_json::from_value(node.clone()).unwrap_or_default()
}

/// 保存设置：读现有 JSON → 补丁 DaemonConfig 全部字段（保留未知字段）→ 原子写回。
pub fn save_config(cfg: &DaemonConfig) -> io::Result<()> {
    let Some(path) = config_path() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "无 LOCALAPPDATA，无法写配置",
        ));
    };
    // 读现有（剥 BOM/注释；失败 → 空对象）。保留未知字段：keymap/max_candidates 等。
    let mut root: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .map(|t| {
            let t = t.trim_start_matches('\u{FEFF}');
            strip_jsonc_comments(t)
        })
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if let Some(obj) = root.as_object_mut() {
        obj.insert("theme".into(), serde_json::Value::String(cfg.theme.clone()));
        obj.insert(
            "candidate_orientation".into(),
            serde_json::Value::String(cfg.candidate_orientation.clone()),
        );
        let ps = cfg.page_size.clamp(5, 9);
        obj.insert("page_size".into(), serde_json::Value::from(ps as u64));
        obj.insert(
            "initial_state".into(),
            serde_json::to_value(&cfg.initial_state)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?,
        );
        obj.insert(
            "passthrough_apps".into(),
            serde_json::Value::Array(
                cfg.passthrough_apps
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
        obj.insert(
            "candidate_owner_apps".into(),
            serde_json::Value::Array(
                cfg.candidate_owner_apps
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
        obj.insert(
            "disabled_log_modules".into(),
            serde_json::Value::Array(
                cfg.disabled_log_modules
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
        obj.insert(
            "keymap".into(),
            serde_json::to_value(&cfg.keymap)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?,
        );
        // 旧顶层 english_punctuation 键清理（迁移后由新 initial_state.punct 取代）。
        obj.remove("english_punctuation");
    } else {
        // 现有文件顶层非对象（异常）→ 重建。
        root = serde_json::json!({
            "theme": cfg.theme,
            "candidate_orientation": cfg.candidate_orientation,
            "page_size": cfg.page_size.clamp(5, 9),
            "initial_state": cfg.initial_state,
            "passthrough_apps": cfg.passthrough_apps,
            "candidate_owner_apps": cfg.candidate_owner_apps,
            "disabled_log_modules": cfg.disabled_log_modules,
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
        save_config(&DaemonConfig {
            theme: "dark".into(),
            candidate_orientation: "horizontal".into(),
            page_size: 7,
            initial_state: iuv_core::ImeState {
                mode: iuv_core::InitialMode::Chinese,
                width: iuv_core::WidthMode::Half,
                script: iuv_core::ScriptMode::Simplified,
                punct: iuv_core::PunctMode::English,
            },
            passthrough_apps: vec!["notepad.exe".to_string()],
            candidate_owner_apps: vec!["wow.exe".to_string()],
            disabled_log_modules: vec!["uielem".to_string()],
            keymap: iuv_core::Keymap {
                toggle_mode: iuv_core::TwoSlot::from(iuv_core::Combo::plain(iuv_core::Key::F5)),
                ..iuv_core::Keymap::default()
            },
        })
        .unwrap();
        // 未知字段保留
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["page_size"], 7);
        // keymap 写为两槽对象（覆盖旧数组格式）
        assert_eq!(v["keymap"]["page_prev"]["primary"], "PageUp");
        assert_eq!(v["keymap"]["toggle_mode"]["primary"], "F5");
        // 已知字段更新
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["candidate_orientation"], "horizontal");
        assert_eq!(v["initial_state"]["mode"], "chinese");
        assert_eq!(v["initial_state"]["width"], "half");
        assert_eq!(v["initial_state"]["script"], "simplified");
        assert_eq!(v["initial_state"]["punct"], "english");
        assert_eq!(v["passthrough_apps"][0], "notepad.exe");
        assert_eq!(v["candidate_owner_apps"][0], "wow.exe");
        assert_eq!(v["disabled_log_modules"][0], "uielem");
        // 旧顶层 english_punctuation 键清理（无残留）
        assert!(v.get("english_punctuation").is_none());
        // 重新加载
        let cfg = load_config();
        assert_eq!(cfg.theme, "dark");
        assert_eq!(cfg.candidate_orientation, "horizontal");
        assert_eq!(cfg.page_size, 7);
        assert_eq!(
            cfg.initial_state.punct,
            iuv_core::PunctMode::English,
            "英文标点往返"
        );
        assert_eq!(cfg.passthrough_apps, vec!["notepad.exe".to_string()]);
        assert_eq!(cfg.candidate_owner_apps, vec!["wow.exe".to_string()]);
        assert_eq!(cfg.disabled_log_modules, vec!["uielem".to_string()]);
        // keymap 往返：F5 全局热键读回
        assert_eq!(
            cfg.keymap.toggle_mode.primary,
            Some(iuv_core::Combo::plain(iuv_core::Key::F5))
        );
        assert_eq!(cfg.keymap.page_prev.primary, Some(iuv_core::Combo::plain(iuv_core::Key::PageUp)));
        let _ = std::env::remove_var("LOCALAPPDATA");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_legacy_english_punctuation_migrates() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("iuv-daemon-config-legacy-{}", std::process::id()));
        let iuv_dir = dir.join("iuv");
        std::fs::create_dir_all(&iuv_dir).unwrap();
        // 旧顶层 english_punctuation 键（升级前配置）→ 迁移并入 initial_state.punct
        std::fs::write(
            iuv_dir.join("config.json"),
            r#"{ "english_punctuation": true, "theme": "dark" }"#,
        )
        .unwrap();
        std::env::set_var("LOCALAPPDATA", &dir);
        let cfg = load_config();
        assert_eq!(
            cfg.initial_state.punct,
            iuv_core::PunctMode::English,
            "旧键 true → 英文标点"
        );
        assert_eq!(cfg.initial_state.mode, iuv_core::InitialMode::Chinese, "其余默认");
        assert_eq!(cfg.theme, "dark");
        let _ = std::env::remove_var("LOCALAPPDATA");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_clamps_page_size() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("iuv-daemon-config-ps-{}", std::process::id()));
        let iuv_dir = dir.join("iuv");
        std::fs::create_dir_all(&iuv_dir).unwrap();
        // 越界 page_size 钳回 5..=9
        std::fs::write(iuv_dir.join("config.json"), r#"{ "page_size": 3 }"#).unwrap();
        std::env::set_var("LOCALAPPDATA", &dir);
        let cfg = load_config();
        assert_eq!(cfg.page_size, 5, "3 钳回下界 5");
        std::fs::write(iuv_dir.join("config.json"), r#"{ "page_size": 12 }"#).unwrap();
        let cfg = load_config();
        assert_eq!(cfg.page_size, 9, "12 钳回上界 9");
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
        assert_eq!(cfg.page_size, 5, "缺省每页 5 条");
        assert_eq!(
            cfg.initial_state,
            iuv_core::ImeState::default(),
            "缺省初始状态 = 中文/半角/简体/中文标点"
        );
        assert!(cfg.passthrough_apps.is_empty());
        assert!(cfg.candidate_owner_apps.is_empty(), "缺省候选自绘名单为空（恒自绘）");
        assert!(cfg.disabled_log_modules.is_empty(), "缺省字段默认全记录");
        let _ = std::env::remove_var("LOCALAPPDATA");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 默认名单常量契约（设置页「恢复默认名单」与脚本模板的回填/生成源）：
    /// 非空、ASCII exe 名——防手滑清空或混入非法条目。
    #[test]
    fn default_app_lists_contract() {
        // 实测锚点：WoW 双画 / 2077 用户实测，防手滑清空
        assert!(DEFAULT_CANDIDATE_OWNER_APPS.contains(&"wow.exe"));
        assert!(DEFAULT_PASSTHROUGH_APPS.contains(&"Cyberpunk2077.exe"));
        for (name, list) in [
            ("候选自绘", &DEFAULT_CANDIDATE_OWNER_APPS[..]),
            ("按键直通", &DEFAULT_PASSTHROUGH_APPS[..]),
        ] {
            assert!(!list.is_empty(), "{name}默认名单不应为空");
            assert!(
                list.iter()
                    .all(|s| s.ends_with(".exe") && s.chars().all(|c| c.is_ascii())),
                "{name}名单须为 ASCII .exe 名：{list:?}"
            );
            assert!(
                list.iter().all(|s| !s.trim().is_empty()),
                "{name}名单不得含空条目"
            );
        }
    }
}