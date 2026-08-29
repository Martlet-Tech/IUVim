//! 桌宠连续物理动画（**纯逻辑**：无 I/O、无时钟、无平台依赖，可单测）。
//!
//! 与 [`crate::pet_model::PetModel`] 的分工（二者并列共存、互不侵入，仅在渲染阶段叠加）：
//!
//! - `PetModel` 负责**离散动作语义**：Idle / Typing / React / StateFlash，决定表情基线与帧号
//! - `PetAnim`（本模块）负责**连续物理**：各图层弹簧摆动、呼吸起伏、随机眨眼
//!
//! 由外部（daemon）喂 `dt_ms` 驱动，本模块自身不读时钟、不持有随机源：
//! 随机性由调用方通过 `rand_u32` 注入，保证纯函数语义与可测试性。
//!
//! # 省电契约
//!
//! 静止时（呼吸关闭 + 所有弹簧收敛）[`PetAnim::needs_tick`] 返回 `false`，外部应 `KillTimer`
//! 完全停帧。[`PetAnim::desired_interval_ms`] 给出建议帧率：物理活跃时 30fps 跟手，
//! 仅呼吸时降到 10fps（呼吸周期 3.5s，10fps 足够平滑）。

use crate::pet_skin::{LayerId, PetSkin, SpringParam, DEFAULT_DAMPING, DEFAULT_STIFFNESS};

/// dt 钳制上限（毫秒）：休眠唤醒 / 调试卡顿的大 dt 会让物理炸开，钳到 100ms。
pub const DT_CLAMP_MS: u32 = 100;
/// 单次积分子步上限（毫秒）：见 [`Spring::step`] 的稳定性说明，超过该值会发散。
const MAX_SUBSTEP_MS: u32 = 8;
/// 单次闭眼持续时长（毫秒）。
///
/// 注意：该值大于 [`DT_CLAMP_MS`]，故一次闭眼在低帧率（10fps）下需两帧才结束。
pub const BLINK_DURATION_MS: u32 = 120;
/// 物理活跃时的建议帧间隔（≈30fps）。
pub const FAST_INTERVAL_MS: u32 = 33;
/// 仅呼吸时的建议帧间隔（10fps）。
pub const SLOW_INTERVAL_MS: u32 = 100;

/// 弹簧收敛判定阈值（value 与 velocity 同时低于该值视为静止）。
const SETTLE_EPS: f32 = 1e-3;

/// 弹簧阻尼振子（半隐式欧拉积分）。
///
/// 半隐式（symplectic）欧拉比显式欧拉稳定：先更新速度再更新位置，能量不会单调发散。
/// 输出 `value` 是**无量纲**量（约 -1..1），实际摆角由调用方乘 `max_angle_deg` 得到。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    value: f32,
    velocity: f32,
    stiffness: f32,
    damping: f32,
}

impl Spring {
    /// 构造（静止状态）。非法参数（非有限值 / 刚度非正 / 阻尼为负）回落到默认值，不 panic。
    pub fn new(stiffness: f32, damping: f32) -> Self {
        let stiffness =
            if stiffness.is_finite() && stiffness > 0.0 { stiffness } else { DEFAULT_STIFFNESS };
        let damping = if damping.is_finite() && damping >= 0.0 {
            damping
        } else {
            DEFAULT_DAMPING
        };
        Spring { value: 0.0, velocity: 0.0, stiffness, damping }
    }

    /// 注入速度冲量（窗口拖拽的惯性激励）。非有限值忽略。
    pub fn impulse(&mut self, v: f32) {
        if !v.is_finite() {
            return;
        }
        self.velocity += v;
        if !self.velocity.is_finite() {
            self.velocity = 0.0;
        }
    }

    /// 推进 `dt_ms`（先钳制到 [`DT_CLAMP_MS`]，再切成子步积分）。
    ///
    /// 积分（每子步）：
    /// ```text
    /// accel    = -k * value - c * velocity
    /// velocity += accel * dt
    /// value    += velocity * dt
    /// ```
    ///
    /// **为何要子步进**：半隐式欧拉的稳定性依赖步长，且阻尼项会收紧稳定域。
    /// 以默认参数 k=180 / c=12 验算 dt=100ms 的迭代矩阵，特征值 `|λ| ≈ 1.17 > 1`
    /// ——直接积分会发散（实测 100 步后位移爆炸到 5e5）。故统一切成
    /// [`MAX_SUBSTEP_MS`] 以内的子步：8ms 下 `|λ| ≈ 0.95 < 1`，稳定且更精确。
    pub fn step(&mut self, dt_ms: u32) {
        let mut remaining = dt_ms.min(DT_CLAMP_MS);
        while remaining > 0 {
            let sub = remaining.min(MAX_SUBSTEP_MS);
            self.integrate(sub as f32 / 1000.0);
            remaining -= sub;
        }
    }

    /// 单个子步积分（秒）。非有限值兜底归零，避免污染后续子步。
    fn integrate(&mut self, dt: f32) {
        let accel = -self.stiffness * self.value - self.damping * self.velocity;
        self.velocity += accel * dt;
        self.value += self.velocity * dt;
        if !self.value.is_finite() || !self.velocity.is_finite() {
            self.value = 0.0;
            self.velocity = 0.0;
        }
    }

    /// 当前位移（无量纲）。
    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }

    /// 当前速度。
    #[inline]
    pub fn velocity(&self) -> f32 {
        self.velocity
    }

    /// 是否已静止（位移与速度均低于 `eps`），供外部判断是否可停 tick。
    #[inline]
    pub fn is_settled(&self, eps: f32) -> bool {
        self.value.abs() < eps && self.velocity.abs() < eps
    }
}

/// 在区间 `[lo, hi]`（闭区间）内取伪随机值。
///
/// `hi <= lo` 时返回 `lo`（防御外部皮肤填错区间）；结果至少为 1，避免 0 间隔疯狂触发。
fn pick_interval(interval: (u32, u32), rand: u32) -> u32 {
    let (lo, hi) = interval;
    if hi <= lo {
        return lo.max(1);
    }
    let span = hi - lo + 1;
    lo + (rand % span)
}

/// 眨眼计时器（倒计时触发闭眼，闭眼固定时长后重新安排下次）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlinkTimer {
    /// 距下次闭眼的剩余毫秒
    next_in_ms: u32,
    /// 本次闭眼剩余毫秒（0 = 未闭眼）
    closed_ms: u32,
}

impl BlinkTimer {
    /// 构造，首次间隔由 `rand` 在区间内取值。
    pub fn new(interval: (u32, u32), rand: u32) -> Self {
        BlinkTimer { next_in_ms: pick_interval(interval, rand), closed_ms: 0 }
    }

    /// 推进 `dt_ms`；`rand` 仅在**重新安排下次间隔**时被消费。
    pub fn step(&mut self, dt_ms: u32, interval: (u32, u32), rand: u32) {
        let dt = dt_ms.min(DT_CLAMP_MS);
        if self.closed_ms > 0 {
            self.closed_ms = self.closed_ms.saturating_sub(dt);
            if self.closed_ms == 0 {
                self.next_in_ms = pick_interval(interval, rand);
            }
            return;
        }
        self.next_in_ms = self.next_in_ms.saturating_sub(dt);
        if self.next_in_ms == 0 {
            self.closed_ms = BLINK_DURATION_MS;
        }
    }

    /// 是否处于闭眼状态。
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.closed_ms > 0
    }

    /// 强制立即闭眼（如点击互动需要配合表情时）。
    pub fn blink_now(&mut self) {
        self.closed_ms = BLINK_DURATION_MS;
        self.next_in_ms = 0;
    }

    /// 本次闭眼剩余毫秒（仅供测试断言，生产代码不应依赖）。
    #[cfg(test)]
    pub(crate) fn closed_ms_for_test(&self) -> u32 {
        self.closed_ms
    }
}

/// 单图层的弹簧状态（弹簧本体 + 该层的摆动参数）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct LayerSpring {
    spring: Spring,
    param: SpringParam,
}

/// 桌宠连续动画状态机（各图层弹簧 + 呼吸 + 眨眼）。
///
/// 由 [`PetSkin`] 构造：只为配置中带 `spring` 的图层创建弹簧，其余层查角度时返回 0。
#[derive(Clone, Debug)]
pub struct PetAnim {
    springs: [Option<LayerSpring>; LayerId::COUNT],
    breath_t_ms: u32,
    breath_amp: f32,
    breath_period_ms: u32,
    blink: BlinkTimer,
    blink_interval: (u32, u32),
}

impl PetAnim {
    /// 按皮肤描述构造（无摆动参数的图层不建弹簧）。
    pub fn new(skin: &PetSkin) -> Self {
        let mut springs = [None; LayerId::COUNT];
        for layer in &skin.layers {
            if let Some(param) = layer.spring {
                springs[layer.id.index()] = Some(LayerSpring {
                    spring: Spring::new(param.stiffness, param.damping),
                    param,
                });
            }
        }
        let breath_amp = if skin.breath_amp.is_finite() && skin.breath_amp > 0.0 {
            skin.breath_amp
        } else {
            0.0
        };
        let blink_interval = skin.blink_interval_ms;
        PetAnim {
            springs,
            breath_t_ms: 0,
            breath_amp,
            breath_period_ms: skin.breath_period_ms.max(1),
            blink: BlinkTimer::new(blink_interval, 0),
            blink_interval,
        }
    }

    /// 推进一帧。`rand_u32` 由外部喂入（仅眨眼重新排期时消费），保持纯逻辑可测。
    pub fn step(&mut self, dt_ms: u32, rand_u32: u32) {
        let dt = dt_ms.min(DT_CLAMP_MS);
        for layer in self.springs.iter_mut().flatten() {
            layer.spring.step(dt);
        }
        // 呼吸相位取模循环，避免长时间运行后 u32 溢出
        self.breath_t_ms = (self.breath_t_ms + dt) % self.breath_period_ms;
        self.blink.step(dt, self.blink_interval, rand_u32);
    }

    /// 注入整体运动冲量（窗口拖拽惯性），按各层 `influence` 分配。
    ///
    /// 非有限值忽略；分配后各层摆动幅度不同，形成"呆毛甩得最凶"的层次感。
    pub fn impulse(&mut self, v: f32) {
        if !v.is_finite() {
            return;
        }
        for layer in self.springs.iter_mut().flatten() {
            layer.spring.impulse(v * layer.param.influence);
        }
    }

    /// 该图层当前摆角（**度**）。无弹簧层返回 0。
    ///
    /// 对弹簧输出做 `clamp(-1, 1)` 后再乘 `max_angle_deg`，保证不超过配置上限。
    pub fn layer_angle(&self, id: LayerId) -> f32 {
        self.springs[id.index()]
            .map(|l| l.spring.value().clamp(-1.0, 1.0) * l.param.max_angle_deg)
            .unwrap_or(0.0)
    }

    /// 呼吸偏移（**归一化**，相对显示高度；-amp..+amp）。
    ///
    /// 渲染层用它乘宠物显示高度得到像素偏移，作用于身体/头部。
    pub fn breath_offset(&self) -> f32 {
        if self.breath_amp == 0.0 {
            return 0.0;
        }
        let phase = (self.breath_t_ms as f32 / self.breath_period_ms as f32)
            * std::f32::consts::TAU;
        phase.sin() * self.breath_amp
    }

    /// 是否正闭眼（渲染层据此把表情覆盖为 [`crate::pet_skin::FaceExpr::Blink`]）。
    #[inline]
    pub fn is_blinking(&self) -> bool {
        self.blink.is_closed()
    }

    /// 是否有弹簧尚未收敛（决定建议帧率）。
    pub fn spring_active(&self) -> bool {
        self.springs
            .iter()
            .flatten()
            .any(|l| !l.spring.is_settled(SETTLE_EPS))
    }

    /// 是否需要继续 tick：`false` 时外部应 `KillTimer` 完全停帧（省电）。
    ///
    /// 呼吸开启时恒为 `true`（呼吸是常驻连续动画）；呼吸关闭且弹簧收敛后才真正静止。
    #[inline]
    pub fn needs_tick(&self) -> bool {
        self.breath_amp > 0.0 || self.spring_active()
    }

    /// 建议帧间隔（毫秒）：物理活跃 → [`FAST_INTERVAL_MS`]；仅呼吸 → [`SLOW_INTERVAL_MS`]。
    #[inline]
    pub fn desired_interval_ms(&self) -> u32 {
        if self.spring_active() {
            FAST_INTERVAL_MS
        } else {
            SLOW_INTERVAL_MS
        }
    }

    /// 强制立即眨眼（点击互动等场景）。
    pub fn blink_now(&mut self) {
        self.blink.blink_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 推进直到所有弹簧收敛或达到上限，返回实际步数。
    fn settle(anim: &mut PetAnim, step_ms: u32, max_steps: u32) -> u32 {
        for i in 0..max_steps {
            anim.step(step_ms, 0);
            if !anim.spring_active() {
                return i + 1;
            }
        }
        max_steps
    }

    // ===== Spring =====

    #[test]
    fn spring_starts_at_rest() {
        let s = Spring::new(DEFAULT_STIFFNESS, DEFAULT_DAMPING);
        assert_eq!(s.value(), 0.0);
        assert_eq!(s.velocity(), 0.0);
        assert!(s.is_settled(SETTLE_EPS));
    }

    #[test]
    fn spring_impulse_then_settles_back_to_zero() {
        let mut s = Spring::new(DEFAULT_STIFFNESS, DEFAULT_DAMPING);
        s.impulse(5.0);
        assert!(!s.is_settled(SETTLE_EPS), "刚注入冲量不应是静止的");
        // 16ms/步，约 3.2 秒足够欠阻尼系统衰减到阈值以下
        for _ in 0..200 {
            s.step(16);
        }
        assert!(s.is_settled(SETTLE_EPS), "应回落到静止，实际 v={} vel={}", s.value(), s.velocity());
        assert!(s.value().abs() < 0.01);
    }

    #[test]
    fn spring_is_underdamped_and_overshoots() {
        // 欠阻尼（ζ<1）必须越过零点，否则不会有自然的来回摆动感
        let mut s = Spring::new(DEFAULT_STIFFNESS, DEFAULT_DAMPING);
        s.impulse(5.0);
        let mut saw_positive = false;
        let mut saw_negative = false;
        for _ in 0..120 {
            s.step(16);
            if s.value() > 1e-4 {
                saw_positive = true;
            }
            if s.value() < -1e-4 {
                saw_negative = true;
            }
        }
        assert!(saw_positive && saw_negative, "欠阻尼应越过零点来回摆，实际只看到一侧");
    }

    #[test]
    fn spring_dt_is_clamped() {
        // 10 秒一次性喂入：钳到 100ms，位移应与单步 100ms 完全一致
        let mut a = Spring::new(DEFAULT_STIFFNESS, DEFAULT_DAMPING);
        a.impulse(5.0);
        a.step(10_000);
        let mut b = Spring::new(DEFAULT_STIFFNESS, DEFAULT_DAMPING);
        b.impulse(5.0);
        b.step(100);
        assert_eq!(a.value(), b.value(), "dt 必须被钳制到 100ms");
        assert_eq!(a.velocity(), b.velocity());
    }

    #[test]
    fn spring_substepping_makes_dt_sizes_consistent() {
        // 同样推进 500ms：粗步(5×100ms) 与细步(50×10ms) 结果应基本一致，
        // 这正是子步进存在的意义（回归防线：若改回单步积分，此处会因发散而失败）
        let mut coarse = Spring::new(DEFAULT_STIFFNESS, DEFAULT_DAMPING);
        let mut fine = Spring::new(DEFAULT_STIFFNESS, DEFAULT_DAMPING);
        coarse.impulse(5.0);
        fine.impulse(5.0);
        for _ in 0..5 {
            coarse.step(100);
        }
        for _ in 0..50 {
            fine.step(10);
        }
        assert!(coarse.value().is_finite(), "粗步发散了：{}", coarse.value());
        let diff = (coarse.value() - fine.value()).abs();
        assert!(diff < 0.05, "不同步长结果应一致，实际差值 {diff}");
    }

    #[test]
    fn spring_survives_nan_and_invalid_params() {
        let mut s = Spring::new(f32::NAN, f32::NAN);
        s.impulse(f32::NAN);
        assert_eq!(s.value(), 0.0, "非法参数不应污染状态");
        for _ in 0..10 {
            s.step(16);
        }
        assert!(s.value().is_finite() && s.velocity().is_finite());

        // 负刚度/负阻尼回落到默认，不应发散
        let mut neg = Spring::new(-1.0, -1.0);
        neg.impulse(3.0);
        for _ in 0..50 {
            neg.step(16);
        }
        assert!(neg.value().is_finite(), "负参数不应产生非有限值");
    }

    #[test]
    fn spring_stays_stable_at_max_dt() {
        // 最坏情况：每步都取钳制上限 100ms（ω·dt ≈ 1.34 < 2，半隐式欧拉稳定）
        let mut s = Spring::new(DEFAULT_STIFFNESS, DEFAULT_DAMPING);
        s.impulse(5.0);
        for _ in 0..100 {
            s.step(DT_CLAMP_MS);
        }
        assert!(s.value().is_finite(), "最大 dt 下不得发散");
        assert!(s.value().abs() < 1.0, "最大 dt 下应收敛，实际 {}", s.value());
    }

    // ===== BlinkTimer =====

    #[test]
    fn blink_timer_fires_after_interval() {
        let mut t = BlinkTimer::new((100, 100), 0);
        assert!(!t.is_closed());
        t.step(99, (100, 100), 0);
        assert!(!t.is_closed(), "未到间隔不应闭眼");
        t.step(1, (100, 100), 0);
        assert!(t.is_closed(), "到达间隔应闭眼");
        assert_eq!(t.closed_ms_for_test(), BLINK_DURATION_MS);
    }

    #[test]
    fn blink_timer_recovers_after_duration() {
        let mut t = BlinkTimer::new((10, 10), 0);
        t.step(10, (10, 10), 0);
        assert!(t.is_closed());
        // 闭眼时长 120ms > 单帧钳制 100ms，低帧率下需两帧才结束
        t.step(DT_CLAMP_MS, (10, 10), 0);
        assert!(t.is_closed(), "首帧仅推进 100ms，闭眼应仍在持续");
        t.step(DT_CLAMP_MS, (10, 10), 0);
        assert!(!t.is_closed(), "累计超过闭眼时长后应恢复");
    }

    #[test]
    fn blink_pick_interval_stays_in_range() {
        let interval = (2600, 6400);
        for rand in [0u32, 1, 12345, u32::MAX / 2, u32::MAX] {
            let v = pick_interval(interval, rand);
            assert!(
                (2600..=6400).contains(&v),
                "rand={rand} 取到 {v}，超出闭区间 [2600, 6400]"
            );
        }
    }

    #[test]
    fn blink_pick_interval_handles_degenerate_range() {
        assert_eq!(pick_interval((500, 500), 999), 500);
        assert_eq!(pick_interval((900, 100), 999), 900, "hi<=lo 应返回 lo");
        assert_eq!(pick_interval((0, 0), 999), 1, "0 间隔应抬到 1 避免疯狂触发");
    }

    // ===== PetAnim =====

    #[test]
    fn pet_anim_builds_springs_only_for_configured_layers() {
        let skin = PetSkin::builtin_girl_default();
        let anim = PetAnim::new(&skin);
        // 静止时所有层角度为 0（含无弹簧层）
        for id in LayerId::ALL {
            assert_eq!(anim.layer_angle(id), 0.0, "{id:?} 静止时应为 0 度");
        }
    }

    #[test]
    fn pet_anim_impulse_swings_layers_by_influence() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        anim.impulse(3.0);
        anim.step(16, 0);
        let back = anim.layer_angle(LayerId::HairBack).abs();
        let front = anim.layer_angle(LayerId::HairFront).abs();
        let ahoge = anim.layer_angle(LayerId::Ahoge).abs();
        assert!(back > 0.0, "后发应摆动");
        assert!(
            back < front && front < ahoge,
            "摆幅应按 influence 递增：后发({back}) < 前发({front}) < 呆毛({ahoge})"
        );
    }

    #[test]
    fn pet_anim_layer_angle_is_clamped_to_max() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        // 超大冲量：限幅后不得超过配置的 max_angle_deg
        anim.impulse(10_000.0);
        for _ in 0..5 {
            anim.step(16, 0);
        }
        let ahoge_param = skin.layer(LayerId::Ahoge).unwrap().spring.unwrap();
        assert!(
            anim.layer_angle(LayerId::Ahoge).abs() <= ahoge_param.max_angle_deg + f32::EPSILON,
            "摆角必须被 max_angle_deg 限幅，实际 {}",
            anim.layer_angle(LayerId::Ahoge)
        );
    }

    #[test]
    fn pet_anim_no_spring_layer_stays_zero_under_impulse() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        anim.impulse(50.0);
        anim.step(16, 0);
        for id in [LayerId::Body, LayerId::Head, LayerId::Face] {
            assert_eq!(anim.layer_angle(id), 0.0, "{id:?} 无弹簧，不应摆动");
        }
    }

    #[test]
    fn pet_anim_settles_and_stops_needing_fast_tick() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        anim.impulse(4.0);
        assert!(anim.spring_active(), "注入冲量后弹簧应活跃");
        assert_eq!(anim.desired_interval_ms(), FAST_INTERVAL_MS);
        let steps = settle(&mut anim, 16, 400);
        assert!(steps < 400, "弹簧应在 400 步内收敛，实际用了 {steps}");
        assert!(!anim.spring_active());
        assert_eq!(anim.desired_interval_ms(), SLOW_INTERVAL_MS, "收敛后降频到 10fps");
    }

    #[test]
    fn pet_anim_breath_offset_stays_within_amp() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        let amp = skin.breath_amp;
        assert!(amp > 0.0);
        for _ in 0..500 {
            anim.step(16, 0);
            assert!(
                anim.breath_offset().abs() <= amp + f32::EPSILON,
                "呼吸偏移不得超过幅度，实际 {}",
                anim.breath_offset()
            );
        }
    }

    #[test]
    fn pet_anim_breath_reaches_both_extremes() {
        // 呼吸必须真的上下起伏（走过正负两半周期），而非恒定偏移
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        let period = skin.breath_period_ms;
        for _ in 0..(period / 16 + 10) {
            anim.step(16, 0);
            let v = anim.breath_offset();
            min = min.min(v);
            max = max.max(v);
        }
        assert!(min < -0.5 * skin.breath_amp, "应到达呼吸下沿，实际 min={min}");
        assert!(max > 0.5 * skin.breath_amp, "应到达呼吸上沿，实际 max={max}");
    }

    #[test]
    fn pet_anim_breath_phase_wraps_without_overflow() {
        // 长时间推进：相位取模循环，不得溢出或漂移
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        for _ in 0..10_000 {
            anim.step(DT_CLAMP_MS, 0);
        }
        assert!(anim.breath_offset().is_finite());
        assert!(anim.breath_t_ms < skin.breath_period_ms);
    }

    #[test]
    fn pet_anim_needs_tick_false_only_when_fully_idle() {
        let mut skin = PetSkin::builtin_girl_default();
        skin.breath_amp = 0.0; // 关闭呼吸
        let mut anim = PetAnim::new(&skin);
        assert!(!anim.needs_tick(), "无呼吸 + 弹簧静止 → 不需要 tick");
        anim.impulse(5.0);
        assert!(anim.needs_tick(), "弹簧活跃 → 需要 tick");
        settle(&mut anim, 16, 400);
        assert!(!anim.needs_tick(), "收敛后再次静止");
    }

    #[test]
    fn pet_anim_needs_tick_true_with_breath_enabled() {
        let skin = PetSkin::builtin_girl_default();
        let anim = PetAnim::new(&skin);
        assert!(anim.needs_tick(), "呼吸开启时应持续 tick");
    }

    #[test]
    fn pet_anim_ignores_non_finite_impulse() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        anim.impulse(f32::NAN);
        anim.impulse(f32::INFINITY);
        for id in LayerId::ALL {
            assert!(
                anim.layer_angle(id).is_finite(),
                "{id:?} 不应被非有限冲量污染"
            );
        }
        assert!(!anim.spring_active(), "非法冲量不应激活弹簧");
    }

    #[test]
    fn pet_anim_blink_uses_injected_rand() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        let mut blinked = false;
        // 以最大步进推进足够长时间，必然触发一次眨眼
        for i in 0..2000 {
            anim.step(DT_CLAMP_MS, i);
            if anim.is_blinking() {
                blinked = true;
                break;
            }
        }
        assert!(blinked, "长时间推进应至少触发一次眨眼");
    }

    #[test]
    fn pet_anim_blink_now_forces_blink() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        assert!(!anim.is_blinking());
        anim.blink_now();
        assert!(anim.is_blinking(), "blink_now 应立即闭眼");
        anim.step(DT_CLAMP_MS, 0);
        assert!(anim.is_blinking(), "单帧 100ms 不足以结束 120ms 闭眼");
        anim.step(DT_CLAMP_MS, 0);
        assert!(!anim.is_blinking(), "累计超过闭眼时长后恢复");
    }

    #[test]
    fn pet_anim_step_survives_huge_dt() {
        let skin = PetSkin::builtin_girl_default();
        let mut anim = PetAnim::new(&skin);
        anim.impulse(5.0);
        anim.step(u32::MAX, 0);
        for id in LayerId::ALL {
            assert!(anim.layer_angle(id).is_finite(), "{id:?} 角度必须有限");
        }
    }

    #[test]
    fn pet_anim_zero_period_skin_does_not_panic() {
        // 外部皮肤可能填 0 周期，内部必须兜底为 1，避免除零
        let mut skin = PetSkin::builtin_girl_default();
        skin.breath_period_ms = 0;
        let mut anim = PetAnim::new(&skin);
        anim.step(16, 0);
        assert!(anim.breath_offset().is_finite(), "0 周期不得产生 NaN");
    }
}
