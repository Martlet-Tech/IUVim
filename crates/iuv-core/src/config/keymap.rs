//! 快捷键映射（41-keymap-settings.md）。双备选键位（主/备两槽）+ 全局热键独立层。
//!
//! 两层机制完全独立：
//! - **会话内快捷键**（TSF 键 sink）：翻页/候选移动/调权/隐藏。仅无修饰/Shift 组合；
//!   Alt 组合 = WM_SYSKEYDOWN 不进 TSF 键 sink（机制死路）；Ctrl 组合放行给应用（红线）。
//! - **全局热键**（daemon `RegisterHotKey`，普通软件做法）：中英/全角/简繁/标点/设置/工具栏，
//!   Alt/Ctrl 随便绑（与解析用户码字的按键系统完全独立）。
//!
//! 序列化：`Combo` = `"Shift+Left"` / `"Ctrl+Alt+I"` / `","`（修饰序固定 Ctrl/Alt/Shift/Win）。

use crate::Key;

/// 一个可绑定的组合键 = 修饰键集合 + 基础键（无修饰的物理键）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub base: Key,
}

impl Combo {
    /// 无修饰组合（基础键）。
    pub fn plain(base: Key) -> Combo {
        Combo { ctrl: false, alt: false, shift: false, win: false, base }
    }

    /// Shift 组合。
    pub fn shifted(base: Key) -> Combo {
        Combo { ctrl: false, alt: false, shift: true, win: false, base }
    }

    /// 展示名（config.json / 设置页 / 日志）：`"Shift+Left"`、`"Ctrl+Alt+I"`、`","`。
    /// 字母基础键显示为大写（录入展示惯例）；解析时已归一化小写（大小写等价）。
    pub fn name(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.win {
            parts.push("Win".into());
        }
        let base = match self.base {
            Key::Char(c) if c.is_ascii_lowercase() => Key::Char(c.to_ascii_uppercase()),
            k => k,
        };
        parts.push(base.name());
        parts.join("+")
    }

    /// 从展示名解析。基础键字母归一为小写（大写输入等价）；解析失败 → None。
    pub fn from_name(s: &str) -> Option<Combo> {
        let mut c = Combo { ctrl: false, alt: false, shift: false, win: false, base: Key::Space };
        let mut base: Option<Key> = None;
        for part in s.split('+') {
            match part {
                "Ctrl" => c.ctrl = true,
                "Alt" => c.alt = true,
                "Shift" => c.shift = true,
                "Win" => c.win = true,
                _ => {
                    if base.is_some() {
                        return None; // 多个基础键
                    }
                    let mut k = Key::from_name(part)?;
                    // 字母归一化小写（Combo 语义：Shift+字母 由 shift 位表达，base 恒小写）
                    if let Key::Char(ch) = k {
                        if ch.is_ascii_uppercase() {
                            k = Key::Char(ch.to_ascii_lowercase());
                        }
                    }
                    base = Some(k);
                }
            }
        }
        c.base = base?;
        Some(c)
    }

    /// 修饰键是否 >0（全局热键注册前提：必须 ≥1 修饰，否则全系统劫持字母/数字）。
    pub fn has_modifier(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.win
    }

    /// 基础键是否为字母（拼音输入空间——会话快捷键禁止字母，全局热键可配但需修饰）。
    pub fn base_is_letter(&self) -> bool {
        matches!(self.base, Key::Char(c) | Key::ShiftChar(c) if c.is_ascii_alphabetic())
    }
}

impl std::fmt::Display for Combo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

impl serde::Serialize for Combo {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name())
    }
}

impl<'de> serde::Deserialize<'de> for Combo {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Combo::from_name(&raw).ok_or_else(|| serde::de::Error::custom(format!("未知组合键：{raw}")))
    }
}

/// 双槽键位（主/备，任一可空——41-keymap-settings.md §3「每个功能两个备选键位」）。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TwoSlot {
    pub primary: Option<Combo>,
    pub secondary: Option<Combo>,
}

impl TwoSlot {
    /// 两槽迭代（冲突检测/注册用）。
    pub fn iter(&self) -> impl Iterator<Item = &Combo> {
        self.primary.iter().chain(self.secondary.iter())
    }

    /// 是否包含某组合（冲突检测）。
    pub fn contains(&self, combo: &Combo) -> bool {
        self.primary.as_ref() == Some(combo) || self.secondary.as_ref() == Some(combo)
    }
}

/// 会话内快捷键动作（归一化到既有 Key 变体，由 TSF `route_key` 消费）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    PagePrev,
    PageNext,
    CandidatePrev,
    CandidateNext,
    SwapLeft,
    SwapRight,
    HideCandidate,
}

impl SessionAction {
    /// 动作 → 归一化引擎键（喂给 Session）。
    pub fn key(self) -> Key {
        match self {
            SessionAction::PagePrev => Key::PageUp,
            SessionAction::PageNext => Key::PageDown,
            SessionAction::CandidatePrev => Key::Left,
            SessionAction::CandidateNext => Key::Right,
            SessionAction::SwapLeft => Key::SwapLeft,
            SessionAction::SwapRight => Key::SwapRight,
            SessionAction::HideCandidate => Key::HideCandidate,
        }
    }
}

/// 全局热键动作（daemon `RegisterHotKey` 消费；与 TSF 完全独立）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GlobalAction {
    ToggleMode = 0,
    ToggleWidth = 1,
    ToggleScript = 2,
    TogglePunct = 3,
    OpenSettings = 4,
    ToggleToolbar = 5,
}

/// 快捷键映射表。默认值保肌肉记忆（41-keymap-settings.md §3）：
/// 翻页 主=PageUp 备=`,`；候选移动 主=←；调权 = Shift+←/→；隐藏 = Shift+Delete；
/// 全局六项默认**空**（决策点 1：不预占全局键）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Keymap {
    // —— 会话内（TSF 键 sink；仅无修饰/Shift 组合）——
    pub page_prev: TwoSlot,
    pub page_next: TwoSlot,
    pub candidate_prev: TwoSlot,
    pub candidate_next: TwoSlot,
    pub swap_left: TwoSlot,
    pub swap_right: TwoSlot,
    pub hide_candidate: TwoSlot,
    // —— 全局热键（daemon；≥1 修饰键）——
    pub toggle_mode: TwoSlot,
    pub toggle_width: TwoSlot,
    pub toggle_script: TwoSlot,
    pub toggle_punct: TwoSlot,
    pub open_settings: TwoSlot,
    pub toggle_toolbar: TwoSlot,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap {
            page_prev: TwoSlot {
                primary: Some(Key::PageUp.into()),
                secondary: Some(Key::Char(',').into()),
            },
            page_next: TwoSlot {
                primary: Some(Key::PageDown.into()),
                secondary: Some(Key::Char('.').into()),
            },
            candidate_prev: TwoSlot {
                primary: Some(Key::Left.into()),
                secondary: Some(Key::Up.into()),
            },
            candidate_next: TwoSlot {
                primary: Some(Key::Right.into()),
                secondary: Some(Key::Down.into()),
            },
            swap_left: TwoSlot { primary: Some(Combo::shifted(Key::Left)), secondary: None },
            swap_right: TwoSlot { primary: Some(Combo::shifted(Key::Right)), secondary: None },
            hide_candidate: TwoSlot { primary: Some(Combo::shifted(Key::Delete)), secondary: None },
            toggle_mode: TwoSlot::default(),
            toggle_width: TwoSlot::default(),
            toggle_script: TwoSlot::default(),
            toggle_punct: TwoSlot::default(),
            open_settings: TwoSlot::default(),
            toggle_toolbar: TwoSlot::default(),
        }
    }
}

impl Keymap {
    /// 会话动作槽位。
    pub fn session_slot(&self, a: SessionAction) -> &TwoSlot {
        match a {
            SessionAction::PagePrev => &self.page_prev,
            SessionAction::PageNext => &self.page_next,
            SessionAction::CandidatePrev => &self.candidate_prev,
            SessionAction::CandidateNext => &self.candidate_next,
            SessionAction::SwapLeft => &self.swap_left,
            SessionAction::SwapRight => &self.swap_right,
            SessionAction::HideCandidate => &self.hide_candidate,
        }
    }

    /// 全局动作槽位。
    pub fn global_slot(&self, a: GlobalAction) -> &TwoSlot {
        match a {
            GlobalAction::ToggleMode => &self.toggle_mode,
            GlobalAction::ToggleWidth => &self.toggle_width,
            GlobalAction::ToggleScript => &self.toggle_script,
            GlobalAction::TogglePunct => &self.toggle_punct,
            GlobalAction::OpenSettings => &self.open_settings,
            GlobalAction::ToggleToolbar => &self.toggle_toolbar,
        }
    }

    /// 会话组合键 → 归一化动作。命中任一槽 → Some(动作)。
    /// 调用方（TSF）保证只查非字母基础键（字母恒走拼音，见 route_key）。
    pub fn map(&self, combo: &Combo) -> Option<SessionAction> {
        let hit = |slot: &TwoSlot| slot.contains(combo);
        // 顺序即优先级：同时命中多槽时（重复配置）取靠前者，行为确定。
        if hit(&self.page_prev) {
            Some(SessionAction::PagePrev)
        } else if hit(&self.page_next) {
            Some(SessionAction::PageNext)
        } else if hit(&self.candidate_prev) {
            Some(SessionAction::CandidatePrev)
        } else if hit(&self.candidate_next) {
            Some(SessionAction::CandidateNext)
        } else if hit(&self.swap_left) {
            Some(SessionAction::SwapLeft)
        } else if hit(&self.swap_right) {
            Some(SessionAction::SwapRight)
        } else if hit(&self.hide_candidate) {
            Some(SessionAction::HideCandidate)
        } else {
            None
        }
    }

    /// 全局组合键 → 动作（daemon WM_HOTKEY 分派用）。
    pub fn global_action(&self, combo: &Combo) -> Option<GlobalAction> {
        let hit = |slot: &TwoSlot| slot.contains(combo);
        if hit(&self.toggle_mode) {
            Some(GlobalAction::ToggleMode)
        } else if hit(&self.toggle_width) {
            Some(GlobalAction::ToggleWidth)
        } else if hit(&self.toggle_script) {
            Some(GlobalAction::ToggleScript)
        } else if hit(&self.toggle_punct) {
            Some(GlobalAction::TogglePunct)
        } else if hit(&self.open_settings) {
            Some(GlobalAction::OpenSettings)
        } else if hit(&self.toggle_toolbar) {
            Some(GlobalAction::ToggleToolbar)
        } else {
            None
        }
    }

    /// 全表组合迭代（会话 + 全局；设置页冲突检测用）。
    pub fn all_combos(&self) -> Vec<Combo> {
        let mut v: Vec<Combo> = Vec::new();
        let session = [
            SessionAction::PagePrev,
            SessionAction::PageNext,
            SessionAction::CandidatePrev,
            SessionAction::CandidateNext,
            SessionAction::SwapLeft,
            SessionAction::SwapRight,
            SessionAction::HideCandidate,
        ];
        let global = [
            GlobalAction::ToggleMode,
            GlobalAction::ToggleWidth,
            GlobalAction::ToggleScript,
            GlobalAction::TogglePunct,
            GlobalAction::OpenSettings,
            GlobalAction::ToggleToolbar,
        ];
        for a in session {
            v.extend(self.session_slot(a).iter().cloned());
        }
        for a in global {
            v.extend(self.global_slot(a).iter().cloned());
        }
        v
    }
}

impl From<Key> for Combo {
    fn from(base: Key) -> Combo {
        Combo::plain(base)
    }
}

impl From<Combo> for TwoSlot {
    fn from(c: Combo) -> TwoSlot {
        TwoSlot { primary: Some(c), secondary: None }
    }
}

impl From<Key> for TwoSlot {
    fn from(k: Key) -> TwoSlot {
        TwoSlot { primary: Some(k.into()), secondary: None }
    }
}

/// 会话外是否可用该键开启新会话（字母键——小写拼音 / Shift/CapsLock 大写保形进序列；
/// `,`/`.`/`'` 等标点放行给应用——`'` 只在会话内作强制分隔符，开场按 `'` 应直接上屏）。
pub fn is_session_start_key(key: Key) -> bool {
    matches!(key, Key::Char(c) | Key::ShiftChar(c) if c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(s: &str) -> Combo {
        Combo::from_name(s).expect(s)
    }

    #[test]
    fn combo_name_roundtrip() {
        for s in [
            "Shift+Left",
            "Ctrl+Alt+I",
            ",",
            "PageUp",
            "Shift+Delete",
            "Shift+Win+L",
            "Ctrl+0",
            "F5",
        ] {
            assert_eq!(combo(s).name(), s, "roundtrip: {s}");
            assert_eq!(combo(s), combo(&combo(s).name()));
        }
    }

    #[test]
    fn combo_letter_lowercase_normalized() {
        // 基础键字母归一化小写：大写输入等价（Shift 语义由 shift 位表达）
        let a = Combo { ctrl: true, alt: false, shift: false, win: false, base: Key::Char('a') };
        assert_eq!(combo("Ctrl+A"), a);
        assert_eq!(combo("Ctrl+A").name(), "Ctrl+A");
        // 修饰序固定：Ctrl/Alt/Shift/Win + base
        let c = Combo { ctrl: true, alt: true, shift: true, win: true, base: Key::Char('z') };
        assert_eq!(c.name(), "Ctrl+Alt+Shift+Win+Z");
    }

    #[test]
    fn combo_parse_errors() {
        assert!(Combo::from_name("").is_none());
        assert!(Combo::from_name("Shift++Left").is_none());
        assert!(Combo::from_name("Ctrl+Alt").is_none(), "无基础键");
        assert!(Combo::from_name("X+Y").is_none(), "多基础键");
        assert!(Combo::from_name("Nonsense").is_none());
    }

    #[test]
    fn combo_predicates() {
        assert!(combo("Shift+Left").has_modifier());
        assert!(combo("Ctrl+Alt+I").has_modifier());
        assert!(!combo(",").has_modifier());
        assert!(combo("Ctrl+A").base_is_letter());
        assert!(!combo("Ctrl+1").base_is_letter());
        assert!(!combo("Shift+Left").base_is_letter());
    }

    #[test]
    fn defaults() {
        let k = Keymap::default();
        // 翻页 主=PageUp 备=,
        assert_eq!(k.page_prev.primary, Some(combo("PageUp")));
        assert_eq!(k.page_prev.secondary, Some(combo(",")));
        assert_eq!(k.page_next.primary, Some(combo("PageDown")));
        assert_eq!(k.page_next.secondary, Some(combo(".")));
        // 候选移动 主=←/→ 备=↑/↓（保肌肉记忆：map_key 去硬编码后由 keymap 治理）
        assert_eq!(k.candidate_prev.primary, Some(combo("Left")));
        assert_eq!(k.candidate_next.primary, Some(combo("Right")));
        assert_eq!(k.candidate_prev.secondary, Some(combo("Up")));
        assert_eq!(k.candidate_next.secondary, Some(combo("Down")));
        // 调权/隐藏 = 原 Shift 组合（保肌肉记忆）
        assert_eq!(k.swap_left.primary, Some(combo("Shift+Left")));
        assert_eq!(k.swap_right.primary, Some(combo("Shift+Right")));
        assert_eq!(k.hide_candidate.primary, Some(combo("Shift+Delete")));
        // 全局六项默认空（决策点 1）
        assert!(k.toggle_mode.iter().next().is_none());
        assert!(k.toggle_width.iter().next().is_none());
        assert!(k.toggle_script.iter().next().is_none());
        assert!(k.toggle_punct.iter().next().is_none());
        assert!(k.open_settings.iter().next().is_none());
        assert!(k.toggle_toolbar.iter().next().is_none());
    }

    #[test]
    fn session_map() {
        let k = Keymap::default();
        assert_eq!(k.map(&combo("PageUp")), Some(SessionAction::PagePrev));
        assert_eq!(k.map(&combo(",")), Some(SessionAction::PagePrev));
        assert_eq!(k.map(&combo("PageDown")), Some(SessionAction::PageNext));
        assert_eq!(k.map(&combo(".")), Some(SessionAction::PageNext));
        assert_eq!(k.map(&combo("Left")), Some(SessionAction::CandidatePrev));
        assert_eq!(k.map(&combo("Shift+Left")), Some(SessionAction::SwapLeft));
        assert_eq!(k.map(&combo("Shift+Right")), Some(SessionAction::SwapRight));
        assert_eq!(k.map(&combo("Shift+Delete")), Some(SessionAction::HideCandidate));
        assert_eq!(k.map(&combo("Ctrl+A")), None, "字母不入会话查表");
        assert_eq!(k.map(&combo("F5")), None);
    }

    #[test]
    fn session_map_custom() {
        // 自定义会话键：改绑候选移动为 h/l，翻页为 [ ]
        let mut k = Keymap::default();
        k.candidate_prev.primary = Some(combo("h"));
        k.candidate_next.primary = Some(combo("l"));
        k.page_prev.primary = Some(combo("["));
        k.page_next.primary = Some(combo("]"));
        assert_eq!(k.map(&combo("h")), Some(SessionAction::CandidatePrev));
        assert_eq!(k.map(&combo("l")), Some(SessionAction::CandidateNext));
        assert_eq!(k.map(&combo("[")), Some(SessionAction::PagePrev));
        assert_eq!(k.map(&combo("]")), Some(SessionAction::PageNext));
        // 备槽仍生效：page_prev 备槽 `,` 未被主槽覆盖
        assert_eq!(k.map(&combo(",")), Some(SessionAction::PagePrev), "备槽仍生效");
        assert_eq!(k.map(&combo("PageUp")), None, "主槽已被覆盖");
        // 清除后不再命中
        k.candidate_prev.primary = None;
        k.candidate_prev.secondary = None;
        assert_eq!(k.map(&combo("h")), None);
    }

    #[test]
    fn global_action_lookup() {
        let mut k = Keymap::default();
        k.toggle_mode.primary = Some(combo("Alt+`"));
        k.open_settings.primary = Some(combo("Ctrl+Alt+I"));
        k.toggle_toolbar.secondary = Some(combo("Win+Shift+L"));
        assert_eq!(k.global_action(&combo("Alt+`")), Some(GlobalAction::ToggleMode));
        assert_eq!(k.global_action(&combo("Ctrl+Alt+I")), Some(GlobalAction::OpenSettings));
        assert_eq!(k.global_action(&combo("Win+Shift+L")), Some(GlobalAction::ToggleToolbar));
        assert_eq!(k.global_action(&combo("Shift+Left")), None, "会话键不进全局表");
        assert_eq!(k.global_action(&combo("F5")), None);
    }

    #[test]
    fn all_combos_roundtrip() {
        let mut k = Keymap::default();
        k.toggle_mode.primary = Some(combo("Alt+1"));
        k.open_settings.primary = Some(combo("Ctrl+Alt+I"));
        let list = k.all_combos();
        assert!(list.contains(&combo("PageUp")));
        assert!(list.contains(&combo("Shift+Left")));
        assert!(list.contains(&combo("Alt+1")));
        assert!(list.contains(&combo("Ctrl+Alt+I")));
        assert_eq!(list.len(), 13, "会话 11（翻页2×2+移动2×2+调权2+隐藏1）+ 全局 2");
    }

    #[test]
    fn two_slot_semantics() {
        let t: TwoSlot = combo("F5").into();
        assert_eq!(t.primary, Some(combo("F5")));
        assert_eq!(t.secondary, None);
        assert!(t.contains(&combo("F5")));
        assert!(!t.contains(&combo("F6")));
    }

    #[test]
    fn session_start_keys() {
        assert!(is_session_start_key(Key::Char('a')));
        assert!(is_session_start_key(Key::ShiftChar('H')), "Shift/CapsLock 大写同样开会话");
        assert!(!is_session_start_key(Key::Char('\'')));
        assert!(!is_session_start_key(Key::Char(',')));
        assert!(!is_session_start_key(Key::Digit(1)));
        assert!(!is_session_start_key(Key::Space));
    }
}
