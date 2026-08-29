//! 宠物动画状态机（M1 桌宠骨架 · 核心数据层）。
//!
//! 纯逻辑模块：**无 I/O、无时钟依赖、无平台依赖**。`advance` 由外部喂 `dt_ms` 推进帧；
//! `needs_tick` 告诉外部"是否需要 SetTimer"——空闲时返回 false，daemon 立即 KillTimer，
//! 满足 ARCHITECTURE §7 性能目标"空闲停帧（零 tick）"。
//!
//! 设计要点（docs/pet/M1-IMPLEMENTATION.md §3）：
//! - **模型与素材解耦**：本模块产出逻辑 `PetClip`（M2 可扩展），渲染层
//!   `PetSprites::frame()` 负责缺失回退（缺 → `Idle`；Idle 也缺 → `None`）。
//!   默认宠素材"缺哪个动作"都不影响状态机正确性，仅视觉回退。
//! - **一次性动作 + 稳定态**：`PetMotion` 分两类——稳定态（`Idle` / `Typing`）循环，
//!   一次性态（`React` / `StateFlash`）播完回稳定态。回退目标由 `typing` 标志决定。
//! - **四态仅字段实际变化触发 Flash**：`on_ime_state` 比较新值与当前 `look`；任一字段
//!   不变则不触发（防高频切换反复打断）。变化时按 `mode > script > width > punct`
//!   优先级取首个变化的字段作为 `flash_kind`。
//! - **dt clamp**：`advance(dt)` 把 dt 钳到 100ms，防休眠大跳帧。
//!
//! 可单测（无 I/O、无 panic 路径），`cargo test -p iuv-core` 全绿。

use crate::config::{InitialMode, ImeState};

/// 动作片段标识（M1 内置集；M2 起由 mod 素材描述扩展）。
///
/// 渲染层按此查帧；缺失自动回退 `Idle`（由 `PetSprites::frame` 在 iuv-ui 实现）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PetClip {
    /// 闲置（M1 = 静止帧 0，零 tick）。
    Idle,
    /// 打字敲键盘（循环律动）。
    Typing,
    /// 点击互动（一次性跳 / 蹭）。
    React,
    /// 中文模式形象（M1 素材缺 → 回退 Idle）。
    ModeCn,
    /// 英文模式形象（打盹偷瞄；M1 素材缺 → 回退 Idle）。
    ModeEn,
    /// 全/半角切换一闪（一次性）。
    Width,
    /// 简/繁切换一闪（一次性）。
    Script,
    /// 标点切换一闪（一次性）。
    Punct,
}

/// 动作层——区分稳定的"循环/静止"态与可打断的"一次性"态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetMotion {
    /// 静止（循环基底；`Idle` 帧 0 冻结 = 零 CPU）。
    Idle,
    /// 打字循环。
    Typing,
    /// 点击互动（一次性，播完回 Idle/Typing）。
    React,
    /// 四态切换一闪（一次性，播完回 Idle/Typing）。
    StateFlash,
}

/// 帧率常量（模型内常量，M1 锁定——M2 改由素材描述驱动）。
const TYPING_FPS: u32 = 12;
const ONESHOT_FPS: u32 = 10;

/// 一次性动作总帧数（M1 固定）。
const REACT_TOTAL: u32 = 10;
const FLASH_TOTAL: u32 = 8;

/// dt 钳制上限：休眠唤醒 / 调试卡顿等大 dt 一次性塞进会跳穿整段动画，钳到 100ms。
const DT_CLAMP_MS: u32 = 100;

/// 宠物动画状态机（**纯逻辑**：无 I/O、无时钟依赖）。
///
/// 字段：
/// - `look`：当前四态（外观层；`ImeState` 全仓唯一四态表示）
/// - `motion`：当前动作层
/// - `typing`：打字中标志（一次性动作播完后的回退目标）
/// - `frame`：当前 clip 帧索引
/// - `accum_ms`：帧间隔累加器
/// - `flash_kind`：`StateFlash` 具体是哪个状态（Width / Script / Punct / ModeCn / ModeEn）
pub struct PetModel {
    look: ImeState,
    motion: PetMotion,
    typing: bool,
    frame: u32,
    accum_ms: u32,
    flash_kind: PetClip,
}

impl PetModel {
    /// 用给定初始四态构造（Activate 时由 daemon 喂入绑定实例的 `ImeState`）。
    pub fn new(initial: ImeState) -> Self {
        // 初始即稳定态 Idle：look.mode = initial.mode 供后续 ModeCn/ModeEn clip 解析
        // 派生（虽然 Idle 阶段只用 look.mode 判定 ModeEn 剪影）。
        PetModel {
            look: initial,
            motion: PetMotion::Idle,
            typing: false,
            frame: 0,
            accum_ms: 0,
            flash_kind: PetClip::Idle,
        }
    }

    /// 当前四态快照（daemon 在切换 focused 实例时用于决定是否整体重置）。
    pub fn look(&self) -> ImeState {
        self.look
    }

    /// 四态变化：仅当至少一个字段实际变化时触发 `StateFlash`（一次性 8 帧）；
    /// `flash_kind` 按 `mode > script > width > punct` 优先级取首个变化字段。
    ///
    /// 在稳定态（Idle / Typing）触发：在一次性态中收到变化 → 暂不打断当前动画，
    /// 仅记录新 `look`，等一次性态播完回到稳定态后**下一拍**也不会自动重放 Flash
    /// （避免切换四态瞬间 + 一次性动作 + 结束一次性态 = 三连击）。这是设计取舍：
    /// 已发生的视觉反馈（一次性动画）保留，新四态直接走稳定态展示。
    pub fn on_ime_state(&mut self, s: ImeState) {
        if s == self.look {
            return;
        }
        let kind = first_changed_clip(&self.look, &s);
        self.look = s;
        match self.motion {
            PetMotion::Idle | PetMotion::Typing => {
                self.motion = PetMotion::StateFlash;
                self.flash_kind = kind;
                self.accum_ms = 0;
                self.frame = 0;
            }
            // React / StateFlash 一次性态中：不打断（保留在播动画），仅更新外观基线。
            _ => {}
        }
    }

    /// 打字开始 / 结束。
    /// - `active = true` 且当前不在一次性态中 → 切到 `Typing` 循环
    /// - `active = false` 且当前 motion = `Typing` → 切到 `Idle`（停打回静）
    /// - 一次性态中（React / StateFlash）只更新 `typing` 标志，不打断动画；
    ///   播完后回退时会读取新标志决定回 Typing 还是 Idle。
    pub fn on_typing(&mut self, active: bool) {
        self.typing = active;
        match self.motion {
            PetMotion::Idle if active => {
                self.motion = PetMotion::Typing;
                self.accum_ms = 0;
                self.frame = 0;
            }
            PetMotion::Typing if !active => {
                self.motion = PetMotion::Idle;
                self.accum_ms = 0;
                self.frame = 0;
            }
            _ => {}
        }
    }

    /// 点击互动：打断任意动作 → `React`（一次性 10 帧）。
    /// 再次点击 = 重置帧（用户体验：从头跳）。
    pub fn on_click(&mut self) {
        self.motion = PetMotion::React;
        self.accum_ms = 0;
        self.frame = 0;
    }

    /// 时间推进：把 dt 钳到 [`DT_CLAMP_MS`] 喂入帧累加器，按当前 motion 的 fps 换算帧号；
    /// 一次性动作（React/StateFlash）帧号达总帧数时回退稳定态（按 `typing` 标志选
    /// Typing / Idle）。
    ///
    /// `Idle` 帧冻结为 0（零 CPU 路径）：外部 `needs_tick` 返回 false 时根本不该调本方法。
    pub fn advance(&mut self, dt_ms: u32) {
        let dt = dt_ms.min(DT_CLAMP_MS);
        self.accum_ms = self.accum_ms.saturating_add(dt);
        match self.motion {
            PetMotion::Idle => {
                // 冻结帧 0：渲染层读 `frame() == 0` 即可，绝不递增。
                self.accum_ms = 0;
                self.frame = 0;
            }
            PetMotion::Typing => {
                let fps = TYPING_FPS;
                self.frame = (self.accum_ms as u64 * fps as u64 / 1000) as u32;
                // 不重置 accum_ms：循环是"无限"的，长时间累积也不会越界（u32 ≈ 49 天）
                // ——但为防御性保险，每帧 = frame mod clip_len（渲染层负责 clip 长度）。
            }
            PetMotion::React => {
                let fps = ONESHOT_FPS;
                self.frame = (self.accum_ms as u64 * fps as u64 / 1000) as u32;
                let total = REACT_TOTAL;
                if self.frame >= total {
                    self.motion = self.stable_motion();
                    self.frame = 0;
                    self.accum_ms = 0;
                }
            }
            PetMotion::StateFlash => {
                let fps = ONESHOT_FPS;
                self.frame = (self.accum_ms as u64 * fps as u64 / 1000) as u32;
                let total = FLASH_TOTAL;
                if self.frame >= total {
                    self.motion = self.stable_motion();
                    self.frame = 0;
                    self.accum_ms = 0;
                }
            }
        }
    }

    /// 当前解析 clip（渲染层查帧用）。
    ///
    /// - `StateFlash` → `flash_kind`（Width / Script / Punct / ModeCn / ModeEn）
    /// - `Typing` → `PetClip::Typing`
    /// - `React` → `PetClip::React`
    /// - `Idle` → `look.mode == English ? ModeEn : Idle`
    pub fn clip(&self) -> PetClip {
        match self.motion {
            PetMotion::StateFlash => self.flash_kind,
            PetMotion::Typing => PetClip::Typing,
            PetMotion::React => PetClip::React,
            PetMotion::Idle => {
                if self.look.mode == InitialMode::English {
                    PetClip::ModeEn
                } else {
                    PetClip::Idle
                }
            }
        }
    }

    /// 当前帧索引（M1 渲染层按 clip_len 自行 mod）。
    pub fn frame(&self) -> u32 {
        self.frame
    }

    /// 外部是否需要 SetTimer（`false` = 空闲停帧，外部必须 `KillTimer`）。
    ///
    /// 当前实现：仅 `Idle` 静止帧冻结时为 `false`；`Typing` 循环 / `React` 一次性 /
    /// `StateFlash` 一次性都需要 timer 推进。
    pub fn needs_tick(&self) -> bool {
        !matches!(self.motion, PetMotion::Idle)
    }

    /// 一次性动作播完回退目标。
    fn stable_motion(&self) -> PetMotion {
        if self.typing {
            PetMotion::Typing
        } else {
            PetMotion::Idle
        }
    }
}

/// 四态变化时按 `mode > script > width > punct` 优先级取首个变化字段对应的 clip。
///
/// `PetClip::Idle` 不参与此映射（Idle 是"无变化"的语义，不是某个字段的代表）。
fn first_changed_clip(old: &ImeState, new: &ImeState) -> PetClip {
    if old.mode != new.mode {
        return match new.mode {
            InitialMode::Chinese => PetClip::ModeCn,
            InitialMode::English => PetClip::ModeEn,
        };
    }
    if old.script != new.script {
        return PetClip::Script;
    }
    if old.width != new.width {
        return PetClip::Width;
    }
    if old.punct != new.punct {
        return PetClip::Punct;
    }
    // 调用方保证 old != new，但此处兜底：返回 Idle 不会进入 flash（on_ime_state 早退）。
    PetClip::Idle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PunctMode, ScriptMode, WidthMode};

    fn default_state() -> ImeState {
        ImeState::default()
    }

    fn alt_state() -> ImeState {
        ImeState {
            mode: InitialMode::English,
            width: WidthMode::Full,
            script: ScriptMode::Traditional,
            punct: PunctMode::English,
        }
    }

    // ===== on_ime_state =====

    #[test]
    fn on_ime_state_no_change_does_not_trigger_flash() {
        let mut m = PetModel::new(default_state());
        m.on_ime_state(default_state());
        assert_eq!(m.motion, PetMotion::Idle);
        assert!(!m.needs_tick());
    }

    #[test]
    fn on_ime_state_mode_change_triggers_flash_mode_kind() {
        let mut m = PetModel::new(default_state());
        let next = ImeState {
            mode: InitialMode::English,
            ..default_state()
        };
        m.on_ime_state(next);
        assert_eq!(m.motion, PetMotion::StateFlash);
        assert_eq!(m.flash_kind, PetClip::ModeEn);
        assert!(m.needs_tick());
    }

    #[test]
    fn on_ime_state_mode_cn_triggers_mode_cn_kind() {
        let mut m = PetModel::new(ImeState {
            mode: InitialMode::English,
            ..default_state()
        });
        m.on_ime_state(ImeState {
            mode: InitialMode::Chinese,
            ..default_state()
        });
        assert_eq!(m.flash_kind, PetClip::ModeCn);
    }

    #[test]
    fn on_ime_state_priority_mode_over_width() {
        // 多个字段变化时：mode 优先
        let mut m = PetModel::new(default_state());
        m.on_ime_state(ImeState {
            mode: InitialMode::English,
            width: WidthMode::Full,
            ..default_state()
        });
        assert_eq!(m.flash_kind, PetClip::ModeEn);
    }

    #[test]
    fn on_ime_state_priority_script_over_width() {
        // mode 不变时：script 优先于 width
        let mut m = PetModel::new(default_state());
        m.on_ime_state(ImeState {
            script: ScriptMode::Traditional,
            width: WidthMode::Full,
            ..default_state()
        });
        assert_eq!(m.flash_kind, PetClip::Script);
    }

    #[test]
    fn on_ime_state_priority_width_over_punct() {
        // mode/script 不变时：width 优先于 punct
        let mut m = PetModel::new(default_state());
        m.on_ime_state(ImeState {
            width: WidthMode::Full,
            punct: PunctMode::English,
            ..default_state()
        });
        assert_eq!(m.flash_kind, PetClip::Width);
    }

    #[test]
    fn on_ime_state_only_punct_change() {
        let mut m = PetModel::new(default_state());
        m.on_ime_state(ImeState {
            punct: PunctMode::English,
            ..default_state()
        });
        assert_eq!(m.flash_kind, PetClip::Punct);
    }

    #[test]
    fn on_ime_state_during_oneshot_does_not_interupt() {
        // 在 React 一次性态中再收 on_ime_state：保留在 React，仅更新 look。
        let mut m = PetModel::new(default_state());
        m.on_click();
        assert_eq!(m.motion, PetMotion::React);
        m.on_ime_state(ImeState {
            mode: InitialMode::English,
            ..default_state()
        });
        assert_eq!(m.motion, PetMotion::React, "React 一次性态中不被打断");
        assert_eq!(m.look().mode, InitialMode::English, "但 look 仍更新");
    }

    // ===== on_typing =====

    #[test]
    fn on_typing_starts_from_idle() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        assert_eq!(m.motion, PetMotion::Typing);
        assert!(m.needs_tick());
    }

    #[test]
    fn on_typing_false_stops_typing_to_idle() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        m.on_typing(false);
        assert_eq!(m.motion, PetMotion::Idle);
        assert!(!m.needs_tick());
    }

    #[test]
    fn on_typing_redundant_true_is_idempotent() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        let before_frame = m.frame;
        m.on_typing(true);
        assert_eq!(m.motion, PetMotion::Typing);
        assert_eq!(m.frame, before_frame);
    }

    #[test]
    fn on_typing_during_oneshot_updates_flag_only() {
        let mut m = PetModel::new(default_state());
        m.on_click();
        m.on_typing(true);
        assert_eq!(m.motion, PetMotion::React, "React 不被打断");
        // 播完一次后回退时按 typing=true → Typing
        for _ in 0..(REACT_TOTAL * 1000 / ONESHOT_FPS + 200) {
            m.advance(100);
        }
        assert_eq!(m.motion, PetMotion::Typing, "React 播完回 Typing（typing=true）");
    }

    #[test]
    fn on_typing_true_during_oneshot_recovers_to_typing() {
        let mut m = PetModel::new(default_state());
        m.on_click();
        m.on_typing(true);
        // 一次性 action 播完应回 Typing（typing 标志已被置 true）
        for _ in 0..(REACT_TOTAL * 1000 / ONESHOT_FPS + 500) {
            m.advance(100);
        }
        assert_eq!(m.motion, PetMotion::Typing);
    }

    // ===== on_click =====

    #[test]
    fn on_click_from_idle_triggers_react() {
        let mut m = PetModel::new(default_state());
        m.on_click();
        assert_eq!(m.motion, PetMotion::React);
    }

    #[test]
    fn on_click_resets_react_frame() {
        let mut m = PetModel::new(default_state());
        m.on_click();
        m.advance(500); // 推进一段
        assert!(m.frame > 0);
        m.on_click();
        assert_eq!(m.frame, 0, "再次点击重置帧");
    }

    #[test]
    fn on_click_interrupts_typing() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        m.on_click();
        assert_eq!(m.motion, PetMotion::React);
    }

    #[test]
    fn on_click_interrupts_flash() {
        let mut m = PetModel::new(default_state());
        m.on_ime_state(ImeState {
            mode: InitialMode::English,
            ..default_state()
        });
        assert_eq!(m.motion, PetMotion::StateFlash);
        m.on_click();
        assert_eq!(m.motion, PetMotion::React);
    }

    // ===== advance + needs_tick + clip =====

    #[test]
    fn advance_idle_freezes_frame_zero() {
        let mut m = PetModel::new(default_state());
        for _ in 0..10 {
            m.advance(100);
        }
        assert_eq!(m.frame, 0);
        assert!(!m.needs_tick());
    }

    #[test]
    fn advance_typing_advances_frame() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        // 12fps: 100ms tick → frame = 100*12/1000 = 1
        m.advance(100);
        assert_eq!(m.frame, 1);
        // 累计 1000ms（10 ticks × 100ms）→ frame = 12
        for _ in 0..9 {
            m.advance(100);
        }
        assert_eq!(m.frame, 12, "10 × 100ms = 1000ms @ 12fps = 12 帧");
    }

    #[test]
    fn advance_react_returns_to_typing_when_typing_true() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        m.on_click();
        // 推够 REACT_TOTAL 帧时间（10 帧 * 100ms/帧 @10fps = 1000ms = 10 ticks）
        for _ in 0..12 {
            m.advance(100);
        }
        assert_eq!(m.motion, PetMotion::Typing, "React 播完回 Typing");
    }

    #[test]
    fn advance_react_returns_to_idle_when_typing_false() {
        let mut m = PetModel::new(default_state());
        m.on_click();
        for _ in 0..12 {
            m.advance(100);
        }
        assert_eq!(m.motion, PetMotion::Idle, "React 播完回 Idle（typing=false）");
    }

    #[test]
    fn advance_flash_returns_to_stable() {
        let mut m = PetModel::new(default_state());
        m.on_ime_state(ImeState {
            width: WidthMode::Full,
            ..default_state()
        });
        // 8 帧 * 100ms/帧 @10fps = 800ms = 8 ticks
        for _ in 0..10 {
            m.advance(100);
        }
        assert_eq!(m.motion, PetMotion::Idle, "Flash 播完回 Idle");
    }

    #[test]
    fn advance_dt_clamp_prevents_huge_jump() {
        // 10 秒一次性塞进：clamp 100ms，只推 1 帧。
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        m.advance(10_000);
        // 12fps * 100ms = 1 帧（钳制后）
        assert_eq!(m.frame, 1, "dt=10000 被钳到 100，frame=1");
    }

    // ===== clip 解析 =====

    #[test]
    fn clip_chinese_idle_returns_idle() {
        let m = PetModel::new(default_state());
        assert_eq!(m.clip(), PetClip::Idle);
    }

    #[test]
    fn clip_english_idle_returns_mode_en() {
        let m = PetModel::new(ImeState {
            mode: InitialMode::English,
            ..default_state()
        });
        assert_eq!(m.clip(), PetClip::ModeEn);
    }

    #[test]
    fn clip_typing_returns_typing() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        assert_eq!(m.clip(), PetClip::Typing);
    }

    #[test]
    fn clip_react_returns_react() {
        let mut m = PetModel::new(default_state());
        m.on_click();
        assert_eq!(m.clip(), PetClip::React);
    }

    #[test]
    fn clip_flash_returns_flash_kind() {
        let mut m = PetModel::new(default_state());
        m.on_ime_state(ImeState {
            width: WidthMode::Full,
            ..default_state()
        });
        assert_eq!(m.clip(), PetClip::Width);
    }

    // ===== needs_tick 矩阵 =====

    #[test]
    fn needs_tick_false_for_idle() {
        let m = PetModel::new(default_state());
        assert!(!m.needs_tick());
    }

    #[test]
    fn needs_tick_true_for_typing_react_flash() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        assert!(m.needs_tick());
        m.on_click();
        assert!(m.needs_tick());
        // 推完 React → Typing（10 帧 @10fps = 1000ms = 10 × 100ms）
        for _ in 0..12 {
            m.advance(100);
        }
        assert!(m.needs_tick());
    }

    // ===== 综合：打字中切四态 → 播完 Flash 回 Typing（不是 Idle） =====

    #[test]
    fn typing_then_ime_state_change_recovers_to_typing() {
        let mut m = PetModel::new(default_state());
        m.on_typing(true);
        m.on_ime_state(ImeState {
            width: WidthMode::Full,
            ..default_state()
        });
        assert_eq!(m.motion, PetMotion::StateFlash);
        // 8 帧 * 100ms = 800ms @10fps
        for _ in 0..10 {
            m.advance(100);
        }
        assert_eq!(m.motion, PetMotion::Typing);
    }

    // ===== alt_state 用来验证所有字段 =====

    #[test]
    fn alt_state_look_persists() {
        let m = PetModel::new(alt_state());
        assert_eq!(m.look(), alt_state());
    }
}
