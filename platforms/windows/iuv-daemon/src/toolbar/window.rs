//! 工具条主窗口（§6；P2.6 自 toolbar.rs 拆出）：渲染/交互/拖拽/看板判定。
//! 仅工具条线程触碰；wnd_proc 经 GWLP_USERDATA 取回本结构。

use std::collections::VecDeque;
use std::mem::size_of;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use iuv_core::{InitialMode, PunctMode, ScriptMode, WidthMode, PetModel};
use iuv_ui::{
    hit_test, pet_alpha_at, render_composite, CompositeSpec, PetRenderSpec, PetSprites,
    TextRenderer, Theme, ToolbarIcons, ToolbarSpec, TB_GEAR, TB_LOGO, TB_MODE, TB_PUNCT, TB_SCRIPT,
    TB_WIDTH, PET_OVERHANG, PET_ZONE_W,
};
use iuv_ui::layout::Rect;
use iuv_win::{ctl_pipe_name, CtlClient, CtlCmd, CtlResult, PipeClient, Request};
use iuv_win::UlwSurface;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSY};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW,
    SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, SWP_NOACTIVATE,
    SWP_NOCOPYBITS, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, GWLP_USERDATA, HTCLIENT,
    HTTRANSPARENT, IDC_ARROW, IDC_HAND, MA_NOACTIVATE, WM_DESTROY, WM_ERASEBKGND, WM_HOTKEY,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT,
    WM_SETCURSOR, WM_TIMER,
};

use super::prefs::{save_pref, ToolbarPref};
use super::tooltip::TooltipWindow;
use super::{
    button_tooltip, clamp_to_work, client_pos, create_window, cursor_screen, current_theme,
    default_pos, in_rounded_rect, BarEvent, Shared, ToolbarInstance, CLASS_BAR, WM_APP_REFRESH,
    WM_MOUSELEAVE,
};
use crate::log;
use crate::state::DaemonState;
/// 工具条窗口（仅工具条线程触碰；wnd_proc 经 GWLP_USERDATA 取回）。
pub(super) struct ToolbarWindow {
    pub(super) hwnd: HWND,
    shared: Arc<Mutex<Shared>>,
    state: Arc<DaemonState>,
    icons: Arc<ToolbarIcons>,
    /// FIFO 事件队列（信号线程/管道线程 push、本线程 drain；显隐决策唯一入口）。
    pending: Arc<Mutex<VecDeque<BarEvent>>>,
    theme: Theme,
    text: Option<TextRenderer>,
    ulw: UlwSurface,
    rows: Vec<Rect>,
    /// 宠物显示矩形（复合坐标；命中 + 拖拽判别用）。`None` = 无素材/未初始化。
    pet_rect: Option<Rect>,
    /// 复合窗口几何（最近一次 frame() 计算；用于 WM_NCHITTEST 不重复公式）。
    composite_w: i32,
    composite_h: i32,
    toolbar_w: i32,
    toolbar_h: i32,
    overhang: i32,
    radius: f32,
    /// 工具栏 Surface 缓存（脏区重绘：仅在 hover/pressed/四态/主题变化时重渲；
    /// 动画 tick 仅重渲宠物帧 + 合成。M1 仅在 frame() 内懒建首帧缓存，零额外字段）。
    visible: bool,
    hover: Option<usize>,
    pressed: Option<usize>,
    /// 拖拽偏移（拖动起点 = 光标 - 窗口位置）；None = 未拖拽。
    drag_offset: Option<(i32, i32)>,
    /// tooltip 窗口（悬停显示按钮说明）。
    tip: TooltipWindow,
    /// M1 桌宠：宠物动画状态机（纯逻辑，无 I/O；工具条线程独占）。
    pet_model: PetModel,
    /// M1 桌宠：默认宠精灵帧缓存（素材失败 → 空集；clip 缺失回退由 iuv-ui 处理）。
    pet_sprites: Arc<PetSprites>,
    /// M1 桌宠：宠物点击/拖拽判别（按下 = 记录光标屏坐标 + 矩形；> 4px 位移 → 拖拽）。
    pet_down: Option<((i32, i32), Rect)>,
}

/// 动画定时器 id（Win32 范围 1..u32::MAX；0/1 留作系统预定义；选不与现有 hotkey id 冲突值）。
const PET_TIMER_ID: usize = 0xBADC0DE;

/// 动画 tick 间隔（ms）：30fps 上限（M1-IMPLEMENTATION §4.3）。
const PET_TIMER_MS: u32 = 33;

/// 宠物按下/拖拽判别阈值（px；§5.3 推荐判别版）。
const PET_DRAG_THRESHOLD: i32 = 4;

/// pet_alpha_at 命中阈值（§5.2：宠物像素点 > 0x20 视为可点击/拖拽）。
const PET_HIT_ALPHA: u8 = 0x20;

impl ToolbarWindow {
    pub(super) fn new(
        shared: Arc<Mutex<Shared>>,
        state: Arc<DaemonState>,
        icons: Arc<ToolbarIcons>,
        pet_sprites: Arc<PetSprites>,
        pending: Arc<Mutex<VecDeque<BarEvent>>>,
    ) -> ToolbarWindow {
        let hwnd = create_window(CLASS_BAR);
        let theme = current_theme(&state);
        let tip = TooltipWindow::new(theme);
        // PetModel 初始四态 = focused 实例当前态（无 focused → 默认 ImeState::default()）。
        // 真正的初始四态在首次 apply_event(FocusGained) 时被 `reset_for_instance` 覆盖。
        let initial_state = shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .focused
            .and_then(|f| {
                shared
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .instances
                    .get(&f)
                    .map(|i| i.state)
            })
            .unwrap_or_default();
        let mut w = ToolbarWindow {
            hwnd,
            shared,
            state,
            icons,
            pending,
            theme,
            text: None,
            ulw: UlwSurface::new(),
            rows: Vec::new(),
            pet_rect: None,
            composite_w: 0,
            composite_h: 0,
            toolbar_w: 0,
            toolbar_h: 0,
            overhang: 0,
            radius: 0.0,
            visible: false,
            hover: None,
            pressed: None,
            drag_offset: None,
            tip,
            pet_model: PetModel::new(initial_state),
            pet_sprites,
            pet_down: None,
        };
        if !hwnd.is_invalid() {
            w.text = Some(TextRenderer::new());
        }
        w
    }

    fn scale(&self) -> f32 {
        if self.hwnd.is_invalid() {
            return 1.0;
        }
        // SAFETY: hdc 由 GetDC 取得，使用后立即 ReleaseDC。
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            if hdc.is_invalid() {
                return 1.0;
            }
            let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSY);
            ReleaseDC(Some(self.hwnd), hdc);
            if dpi <= 0 {
                1.0
            } else {
                dpi as f32 / 96.0
            }
        }
    }

    /// reconcile（WM_APP_REFRESH）：主题热刷新 + drain FIFO（逐条同步执行显隐动作）
    /// + 按最终态重绘。拖拽中零重绘（只由 drag_move 移动窗口）。
    fn reconcile(&mut self) {
        // 主题热刷新（设置页保存后经 state.config 更新；读内存快照，零磁盘 I/O）。
        let t = current_theme(&self.state);
        if t != self.theme {
            self.theme = t;
            self.tip.set_theme(t);
            log::log_line(&format!("[toolbar] 主题热刷新：{}", self.theme.name));
        }
        self.drain_requests();
        if self.visible && self.drag_offset.is_none() {
            self.repaint();
        }
    }

    /// FIFO 串行消费：pop → 同步执行该事件的显隐动作 → 再取下一条（不丢不弃，
    /// 动作各自耗时天然串行；40-toolbar-show-hide-governance.md 纯信号模型）。
    fn drain_requests(&mut self) {
        loop {
            let ev = {
                let mut q = self.pending.lock().unwrap_or_else(|p| p.into_inner());
                match q.pop_front() {
                    Some(e) => e,
                    None => break,
                }
            };
            self.apply_event(ev);
        }
    }

    /// 单条事件应用（纯信号判定，零前台查询——TSF 线程焦点信号即真相源）：
    /// - `FocusGained`：绑定该实例并立即显示（偏好关闭 → 仅绑定；已可见 → 仅重绘换内容）
    /// - `FocusLost`：绑定者本人 → 解绑并立即隐藏；他人 → 仅改表
    fn apply_event(&mut self, ev: BarEvent) {
        match ev {
            BarEvent::FocusGained { pid, tid, state } => {
                let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                // 防御性 upsert：未知实例直接建表（信号即注册——「有一个」语义）。
                sh.instances.insert(
                    (pid, tid),
                    ToolbarInstance { state, active: true },
                );
                let was_bound = sh.focused == Some((pid, tid));
                let pref_visible = sh.visible;
                let prior_focused = sh.focused;
                sh.focused = Some((pid, tid));
                drop(sh);
                // M1 桌宠：切换 focused 实例 → 整体重置 PetModel（避免新实例先收到
                // 旧实例残留的 React/Typing；新实例从 Idle 起步、PetModel.look = 新态）。
                if prior_focused != Some((pid, tid)) {
                    self.pet_model = PetModel::new(state);
                    self.kill_pet_timer();
                } else {
                    // 同实例重复 FocusGained（罕见，如 Alt+Tab 回切）：仅同步 look，
                    // 不打断当前动画（保留视觉连续性）。
                    self.pet_model.on_ime_state(state);
                    self.sync_pet_timer();
                }
                log::log_line(&format!("[toolbar] 激活（{pid}:{tid}）"));
                if !pref_visible {
                    // 偏好关闭：只绑定实例不显示（§32「切回 iuv → 按偏好重新显示」；
                    // 重开走 ToggleVisible 分支的绑定活跃恢复）。
                    log::log_line("[toolbar] 工具条 → 保持隐藏（偏好关闭，仅绑定）");
                } else if !self.visible {
                    log::log_line(&format!(
                        "[toolbar] 工具条 → 显示（绑定 {pid}:{tid}）"
                    ));
                    self.show();
                } else if !was_bound && self.drag_offset.is_none() {
                    self.repaint();
                    self.sync_pet_timer();
                } else {
                    // 即使没改绘制，也按 PetModel 状态同步定时器
                    self.sync_pet_timer();
                }
            }
            BarEvent::FocusLost { pid, tid } => {
                // 设置窗打开时抑制失焦（41-keymap-settings.md §12）：设置窗是 daemon
                // 自家配置 UI——用户打开设置窗 ≠ 离开 iuv 使用，全局热键应继续作用于
                // 打开设置窗前焦点所在的应用。TSF 实例切到设置窗会触发 OnKillThreadFocus
                // → FocusLost，若不抑制则 focused 被清空 → 设置窗里按全局热键
                // 「无 focused 实例，忽略」（日志刷屏，2026-08-28 实测）。设置窗关闭后
                // 焦点回原应用 → OnSetThreadFocus → FocusGained 自然恢复，无需额外校正。
                if self.state.settings_open.load(Ordering::Acquire) {
                    log::log_line(&format!(
                        "[toolbar] 失焦（{pid}:{tid}）但设置窗打开，保留 focused（全局热键继续作用于原应用）"
                    ));
                    return;
                }
                let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(i) = sh.instances.get_mut(&(pid, tid)) {
                    i.active = false;
                }
                let was_bound = sh.focused == Some((pid, tid));
                if was_bound {
                    sh.focused = None;
                }
                drop(sh);
                log::log_line(&format!("[toolbar] 失焦（{pid}:{tid}）"));
                if was_bound && self.visible {
                    log::log_line(&format!(
                        "[toolbar] 工具条 → 隐藏（解绑 {pid}:{tid}）"
                    ));
                    self.hide();
                }
            }
            BarEvent::StateChanged { pid, tid, state } => {
                let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(i) = sh.instances.get_mut(&(pid, tid)) {
                    i.state = state;
                }
                let bound = sh.focused == Some((pid, tid));
                drop(sh);
                // M1 桌宠：四态变化驱动 PetModel（M1 桌宠骨架：四态联动核心入口）。
                if bound {
                    self.pet_model.on_ime_state(state);
                }
                // 绑定实例四态变化且可见 → 重绘换内容（拖拽中留给 reconcile 收尾帧）。
                if bound && self.visible && self.drag_offset.is_none() {
                    self.repaint();
                }
                // 状态变化后：可能进入一次性动作（StateFlash）→ 需要 SetTimer
                self.sync_pet_timer();
            }
            BarEvent::ToggleVisible => {
                let visible = {
                    let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                    sh.visible = !sh.visible;
                    sh.visible
                };
                log::log_line(&format!("[toolbar] 全局显隐偏好 → {visible}"));
                save_pref(&ToolbarPref {
                    visible,
                    pos: self.shared.lock().unwrap_or_else(|p| p.into_inner()).pos,
                });
                if !visible {
                    if self.visible {
                        self.hide();
                    }
                } else {
                    // 重开偏好：绑定实例仍活跃 → 立即恢复显示。
                    let bound_active = {
                        let sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                        sh.focused
                            .is_some_and(|f| sh.instances.get(&f).is_some_and(|i| i.active))
                    };
                    if bound_active && !self.visible {
                        log::log_line("[toolbar] 工具条 → 显示（偏好重开，绑定实例活跃）");
                        self.show();
                    }
                }
            }
            BarEvent::HotkeysChanged => {
                // 全局热键全量重注册（41-keymap-settings.md §4）：先注销再按新配置注册。
                log::log_line("[toolbar] 全局热键变更 → 注销 + 重注册");
                self.reregister_hotkeys();
            }
            BarEvent::CaptureMode(true) => {
                // 录入态：注销全部全局热键（41-keymap-settings.md §12）——RegisterHotKey
                // 系统级抢键，不注销则设置窗录入按已注册热键时按键进 WM_HOTKEY 不进 egui 流。
                log::log_line("[toolbar] 录入态：注销全部全局热键（吸收按键）");
                crate::hotkey::unregister_all(self.hwnd);
            }
            BarEvent::CaptureMode(false) => {
                // 退出录入：按当前配置重注册（若期间保存了 keymap，后续 HotkeysChanged 再全量重注册）。
                log::log_line("[toolbar] 录入结束：重注册全局热键");
                self.reregister_hotkeys();
            }
            BarEvent::TypingState { pid, tid, active } => {
                // M1 桌宠：打字中事件驱动 PetModel。
                // 仅对当前绑定实例生效（与 StateChanged 同款 pid:tid 校验）。
                let bound = self
                    .shared
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .focused
                    == Some((pid, tid));
                if bound {
                    self.pet_model.on_typing(active);
                    self.sync_pet_timer();
                    if self.visible && self.drag_offset.is_none() {
                        self.repaint();
                    }
                }
            }
        }
    }

    /// 同步动画定时器到 PetModel.needs_tick：true → SetTimer；false → KillTimer。
    /// §5.4：show/apply_event/TypingState/StateChanged/on_click 后必须调用。
    /// WM_TIMER 推进后若 needs_tick 变 false 同样 KillTimer（空闲停帧，零 tick）。
    fn sync_pet_timer(&mut self) {
        if self.hwnd.is_invalid() {
            return;
        }
        if self.pet_model.needs_tick() {
            // SAFETY: SetTimer 在窗口创建线程调用；id 复用固定值（重复 SetTimer 同一 id
            // 会重置计时器，符合预期）。
            unsafe {
                let _ = SetTimer(
                    Some(self.hwnd),
                    PET_TIMER_ID,
                    PET_TIMER_MS,
                    None,
                );
            }
        } else {
            self.kill_pet_timer();
        }
    }

    /// 关闭动画定时器（幂等：KillTimer 对未注册 id 静默返回 0/失败）。
    fn kill_pet_timer(&mut self) {
        if self.hwnd.is_invalid() {
            return;
        }
        // SAFETY: KillTimer 在窗口创建线程调用；已注册 id 关闭；未注册 id 静默忽略。
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PET_TIMER_ID);
        }
    }

    /// 宠物点击命中：给定客户区坐标，判断是否落在宠物**不透明**像素上。
    /// 用于 WM_NCHITTEST 与 WM_LBUTTONDOWN 区分"宠物像素（可点）"与"宠物区透明（穿透）"。
    fn pet_pixel_hit(&self, x: i32, y: i32) -> bool {
        let Some(pr) = self.pet_rect else { return false };
        if x < pr.x || x >= pr.x + pr.w || y < pr.y || y >= pr.y + pr.h {
            return false;
        }
        // alpha 阈值：素材中宠物像素 alpha > PET_HIT_ALPHA（0x20）视为可点
        let a = pet_alpha_at(
            &self.pet_sprites,
            self.pet_model.clip(),
            self.pet_model.frame(),
            &pr,
            x as f32,
            y as f32,
        );
        a > PET_HIT_ALPHA
    }

    /// 注销 + 按当前配置重注册全局热键（HotkeysChanged / 退出录入共用）。
    fn reregister_hotkeys(&mut self) {
        let keymap = self
            .state
            .config
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keymap
            .clone();
        crate::hotkey::unregister_all(self.hwnd);
        let (ok, fail) = crate::hotkey::register_all(self.hwnd, &keymap);
        log::log_line(&format!("[toolbar] 全局热键注册：成功 {ok}，失败 {fail}"));
    }

    /// 显示（首显定位：记忆位置（clamp 回工作区）或主屏右下角；ULW 上屏 + SW_SHOWNA 不抢焦点）。
    fn show(&mut self) {
        if self.hwnd.is_invalid() {
            return;
        }
        let Some(surf) = self.frame() else { return };
        let (w, h) = (surf.w as i32, surf.h as i32);
        let pos = {
            let sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            match sh.pos {
                Some(p) => {
                    // 防御：记忆位置越出显示器工作区（拖拽损坏/多屏变化/旧版本 32767 bug）
                    // → clamp 回工作区，绝不渲染到屏幕外（2026-08-21 实测工具栏隐形）。
                    clamp_to_work(p.0, p.1, w, h)
                }
                None => default_pos(w, h),
            }
        };
        self.present(&surf, pos.0, pos.1, w, h);
        // SAFETY: SW_SHOWNA 显示但不激活——绝不抢焦点（点击不打断活动 composition）。
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNA) };
        self.visible = true;
        // 宠物动画：仅在 visible + needs_tick 时 SetTimer（focused 实例 + 一次性动作期）。
        self.sync_pet_timer();
    }

    /// 原位重绘（reconcile / 悬停 / 按下变化）。
    fn repaint(&mut self) {
        if self.hwnd.is_invalid() || !self.visible {
            return;
        }
        let Some(surf) = self.frame() else { return };
        // SAFETY: GetWindowRect 读当前窗口矩形（ULW 窗口位置即上屏位置）。
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_err() {
            return;
        }
        self.present(&surf, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top);
    }

    /// 渲染当前帧 → Surface（**M1 复合**：工具栏 + 宠物区同窗） + 刷新按钮命中矩形 +
    /// 宠物显示矩形。Surface 尺寸 = (toolbar_w + pet_zone_w, toolbar_h + pet_overhang)。
    fn frame(&mut self) -> Option<iuv_ui::Surface> {
        let scale = self.scale();
        let inst = {
            let sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            sh.focused.and_then(|f| sh.instances.get(&f).copied())
        };
        let state = inst.map(|i| i.state).unwrap_or_default();
        let toolbar_spec = ToolbarSpec {
            icons: &self.icons,
            state,
            hover: self.hover,
            pressed: self.pressed,
        };
        // 宠物规格：素材空集（is_empty）→ 整张 Surface 不画宠物（仅工具栏）。否则按
        // PetModel 解析的 clip/frame 渲染。M1 单一默认宠；clips/frame 越界回退
        // 由 iuv-ui::PetSprites::frame 承担。
        let pet_spec = if self.pet_sprites.is_empty() {
            None
        } else {
            Some(PetRenderSpec {
                sprites: &self.pet_sprites,
                clip: self.pet_model.clip(),
                frame: self.pet_model.frame(),
            })
        };
        let composite_spec = CompositeSpec {
            toolbar: &toolbar_spec,
            pet: pet_spec.as_ref(),
        };
        let (surf, rows, pet_rect) = render_composite(&composite_spec, &self.theme, scale);
        if surf.w == 0 || surf.h == 0 {
            return None;
        }
        self.rows = rows;
        self.pet_rect = pet_rect;
        // 缓存复合几何（供 WM_NCHITTEST 不重复公式）
        self.composite_w = surf.w as i32;
        self.composite_h = surf.h as i32;
        // toolbar_w/toolbar_h = composite_w - pet_zone_w;overhang = composite_h - toolbar_h
        self.overhang = (PET_OVERHANG * scale).ceil() as i32;
        self.toolbar_w = self.composite_w - (PET_ZONE_W * scale).ceil() as i32;
        self.toolbar_h = self.composite_h - self.overhang;
        self.radius = self.theme.corner_radius * scale;
        Some(surf)
    }

    fn present(&mut self, surf: &iuv_ui::Surface, x: i32, y: i32, w: i32, h: i32) {
        self.ulw.upload(self.hwnd, surf, x, y, w, h, "[toolbar]");
    }

    fn hide(&mut self) {
        self.visible = false;
        self.hover = None;
        self.pressed = None;
        // 宠物按下判别状态一并清（防 hide 后再 WM_LBUTTONUP 误触发）
        self.pet_down = None;
        self.tip.hide();
        // 隐藏必停动画定时器（§5.4：避免定时器在窗口隐藏态空转）
        self.kill_pet_timer();
        if !self.hwnd.is_invalid() {
            // SAFETY: 隐藏工具条窗口
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    /// 按钮点击（§6.5 严格请求/响应）：四态钮 → 连接 focused 实例控制管道发 SetState →
    /// 按结果更新实例表 + 重绘（成功/失败分别呈现）；齿轮 → OpenSettings；logo → 拖动把手。
    fn on_click(&mut self, index: usize) {
        let focused = self
            .shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .focused;
        let Some((pid, tid)) = focused else {
            log::log_line("[toolbar] 点击但无 focused 实例，忽略");
            return;
        };
        match index {
            TB_LOGO => {} // 拖动把手（无动作）
            TB_GEAR => {
                log::log_line("[toolbar] 齿轮 → 打开设置页");
                // 设置页通知（独立管道请求，复用 M6 路径）。
                if let Ok(c) = PipeClient::connect() {
                    let _ = c.request(&Request::OpenSettings);
                }
            }
            _ => {
                // 按钮点击 = 该字段双态翻转：读实例表当前态，发目标态（true = 第二态 英/全/繁/英标）。
                let (label, cmd) = {
                    let sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                    let st = sh
                        .instances
                        .get(&(pid, tid))
                        .map(|i| i.state)
                        .unwrap_or_default();
                    match index {
                        TB_MODE => ("中英", CtlCmd::SetMode(st.mode == InitialMode::Chinese)),
                        TB_WIDTH => ("全半角", CtlCmd::SetWidth(st.width == WidthMode::Half)),
                        TB_PUNCT => ("标点", CtlCmd::SetPunct(st.punct == PunctMode::Chinese)),
                        TB_SCRIPT => ("简繁", CtlCmd::SetScript(st.script == ScriptMode::Simplified)),
                        _ => return,
                    }
                };
                self.dispatch_state_toggle(&label, &cmd, pid, tid);
            }
        }
    }

    /// 全局热键触发（WM_HOTKEY → bar_wnd_proc → on_hotkey；41-keymap-settings.md §4）。
    /// 复用 on_click 的 focused → CtlClient 分派：四态 → 连 focused 实例控制管道；
    /// 设置/工具栏显隐 → PipeClient（与语言栏菜单同路径）。
    fn on_hotkey(&mut self, action: crate::hotkey::GlobalAction) {
        let (label, target_cmd) = match action {
            crate::hotkey::GlobalAction::ToggleMode => {
                let st = self.focused_state();
                ("中英", CtlCmd::SetMode(st.mode == InitialMode::Chinese))
            }
            crate::hotkey::GlobalAction::ToggleWidth => {
                let st = self.focused_state();
                ("全半角", CtlCmd::SetWidth(st.width == WidthMode::Half))
            }
            crate::hotkey::GlobalAction::ToggleScript => {
                let st = self.focused_state();
                ("简繁", CtlCmd::SetScript(st.script == ScriptMode::Simplified))
            }
            crate::hotkey::GlobalAction::TogglePunct => {
                let st = self.focused_state();
                ("标点", CtlCmd::SetPunct(st.punct == PunctMode::Chinese))
            }
            crate::hotkey::GlobalAction::OpenSettings => {
                log::log_line("[hotkey] 打开设置页");
                if let Ok(c) = PipeClient::connect() {
                    let _ = c.request(&Request::OpenSettings);
                }
                return;
            }
            crate::hotkey::GlobalAction::ToggleToolbar => {
                log::log_line("[hotkey] 切换工具栏显隐");
                if let Ok(c) = PipeClient::connect() {
                    let _ = c.request(&Request::ToggleToolbar);
                }
                return;
            }
        };
        let focused = self
            .shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .focused;
        let Some((pid, tid)) = focused else {
            log::log_line(&format!("[hotkey] {label} 但无 focused 实例，忽略"));
            return;
        };
        self.dispatch_state_toggle(&label, &target_cmd, pid, tid);
    }

    /// 读当前 focused 实例四态（无实例 → 默认四态）。
    fn focused_state(&self) -> iuv_core::ImeState {
        let sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        sh.focused
            .and_then(|f| sh.instances.get(&f).copied())
            .map(|i| i.state)
            .unwrap_or_default()
    }

    /// 四态翻转分派（on_click 与 on_hotkey 共用）：连 focused 实例控制管道发 cmd →
    /// 按结果更新实例表 + 重绘。
    fn dispatch_state_toggle(&mut self, label: &str, cmd: &CtlCmd, pid: u32, tid: u32) {
        log::log_line(&format!(
            "[toolbar] {label}翻转（实例 {pid}:{tid}）"
        ));
        let name = ctl_pipe_name(pid, tid);
        match CtlClient::connect(&name).and_then(|c| c.request(cmd)) {
            Ok(CtlResult::Ok { state }) => {
                log::log_line(&format!("[toolbar] 实例应用成功：{state:?}"));
                let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(i) = sh.instances.get_mut(&(pid, tid)) {
                    i.state = state;
                }
            }
            Ok(CtlResult::Err { msg }) => {
                log::log_line(&format!("[toolbar] 实例应用失败：{msg}"));
            }
            Err(e) => {
                log::log_line(&format!(
                    "[toolbar] 控制管道不可达（实例离线/未注册？）：{e}"
                ));
            }
        }
        self.repaint();
    }

    /// 悬停更新（WM_MOUSEMOVE 命中）：改 hover 行 + 刷新 tooltip。
    fn on_hover(&mut self, x: i32, y: i32) {
        let hit = hit_test(&self.rows, x, y);
        let changed_hover = hit != self.hover;
        self.hover = hit;
        // tooltip：悬停按钮（logo/齿轮除外）显示说明；离开/空白隐藏。
        if let Some(i) = hit {
            if let Some(label) = button_tooltip(i) {
                if changed_hover {
                    self.tip.show_near(&self.theme, label, self.hwnd);
                }
            } else {
                self.tip.hide();
            }
        } else {
            self.tip.hide();
        }
        if changed_hover {
            self.repaint();
        }
    }

    /// 拖拽开始（空白区/logo 的 WM_LBUTTONDOWN）：记录光标**屏幕坐标**偏移 + 捕获鼠标。
    /// 用屏幕坐标而非客户区坐标：客户区原点随窗口移动而漂移，固定 offset 会造成
    /// 累积滞后/位置漂移（2026-08-21 实测拖到屏幕外 32767 且保存为记忆位置）。
    fn start_drag(&mut self) {
        let (cx, cy) = cursor_screen();
        // SAFETY: GetWindowRect 读当前窗口矩形。
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_err() {
            return;
        }
        self.drag_offset = Some((cx - rc.left, cy - rc.top));
        // SAFETY: SetCapture 捕获鼠标（拖拽期间窗口持续收 WM_MOUSEMOVE）。
        let _ = unsafe { SetCapture(self.hwnd) };
    }

    /// 拖拽移动（WM_MOUSEMOVE）：按光标屏幕坐标差值 SetWindowPos 移动窗口。
    /// **边缘检测**：目标位置先 clamp 到光标所在显示器工作区（x∈[left, right-w]、
    /// y∈[top, bottom-h]）——窗口整体一像素也不越出屏幕（2026-08-21 用户要求）。
    fn drag_move(&mut self) {
        let Some((ox, oy)) = self.drag_offset else { return };
        let (cx, cy) = cursor_screen();
        let (nx, ny) = (cx - ox, cy - oy);
        // SAFETY: GetWindowRect 读当前窗口矩形（窗口尺寸做 clamp 边界）。
        let (nx, ny) = {
            let mut rc = RECT::default();
            if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_ok() {
                clamp_to_work(nx, ny, rc.right - rc.left, rc.bottom - rc.top)
            } else {
                (nx, ny)
            }
        };
        // SAFETY: SWP_NOACTIVATE|NOSIZE|NOZORDER|NOCOPYBITS 仅移动（NOCOPYBITS 防移动时
        // 复制旧客户区位图产生残影）；layered 窗口内容由 DWM 缓存随动。
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                nx,
                ny,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOCOPYBITS,
            )
        };
    }

    /// 拖拽结束（WM_LBUTTONUP）：释放捕获 + 位置 clamp 回工作区 + 持久化。
    fn end_drag(&mut self) {
        self.drag_offset = None;
        // SAFETY: ReleaseCapture 释放 SetCapture 的捕获。
        let _ = unsafe { ReleaseCapture() };
        // SAFETY: GetWindowRect 读最终位置。
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_err() {
            return;
        }
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let pos = clamp_to_work(rc.left, rc.top, w, h);
        if pos.0 != rc.left || pos.1 != rc.top {
            log::log_line(&format!(
                "[toolbar] 拖拽位置越界，clamp 回工作区：({}, {}) → {pos:?}",
                rc.left, rc.top
            ));
        }
        {
            let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            sh.pos = Some(pos);
        }
        save_pref(&ToolbarPref {
            visible: self.shared.lock().unwrap_or_else(|p| p.into_inner()).visible,
            pos: Some(pos),
        });
        log::log_line(&format!("[toolbar] 位置已记忆：{pos:?}"));
    }

    /// M1 桌宠：宠物点击互动（§5.3，未拖拽场景下 WM_LBUTTONUP 触发）。
    /// 打断任意动作 → React（一次性 10 帧）；自然回退到稳定态。
    fn on_pet_click(&mut self) {
        self.pet_model.on_click();
        self.sync_pet_timer();
        // 立即重绘（即使定时器也在跑，首帧不能等下一拍）
        if self.visible && self.drag_offset.is_none() {
            self.repaint();
        }
    }

    /// M1 桌宠：动画 tick 推进（WM_TIMER）。dt 用 33ms（SetTimer 周期）。
    /// 推进后若 needs_tick=false → KillTimer（空闲停帧，零 tick）。
    fn on_pet_tick(&mut self) {
        self.pet_model.advance(PET_TIMER_MS);
        // 推进后：若空闲停帧则 KillTimer；否则保持定时器（继续推进）。
        self.sync_pet_timer();
        // 可见时重绘（一次性态推进关键帧；稳定态在 typing 循环时也需重绘）。
        // 拖拽中不重绘（避免抢 SetWindowPos）。
        if self.visible && self.drag_offset.is_none() {
            self.repaint();
        }
    }
}

impl Drop for ToolbarWindow {
    fn drop(&mut self) {
        // 防定时器在窗口销毁后触发，触碰已释放的 ToolbarWindow（§5.4）。
        self.kill_pet_timer();
        if !self.hwnd.is_invalid() {
            // SAFETY: 先清零 GWLP_USERDATA，杜绝 wnd_proc 访问到即将释放的 self。
            let _ = unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            // SAFETY: 在创建线程（工具条线程）销毁窗口。
            let _ = unsafe { DestroyWindow(self.hwnd) };
            self.hwnd = HWND::default();
        }
    }
}

/// 从 GWLP_USERDATA 取回窗口属主（可变版；调用都在工具条线程）。
fn get_bar_mut(hwnd: HWND) -> Option<&'static mut ToolbarWindow> {
    // SAFETY: 指针在窗口销毁前由 Drop 清零；调用都在创建线程。
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        Some(unsafe { &mut *(p as *mut ToolbarWindow) })
    }
}

/// 工具条窗口过程：定时器/刷新 → reconcile；ULW 内容不画窗口 DC（WM_PAINT 只校验）；
/// 圆角外点击穿透（WM_NCHITTEST）；悬停高亮 + tooltip；空白区/logo 拖拽；按下反馈 + 抬起执行。
/// M1 桌宠：宠物命中 + 点击/拖拽判别 + 动画定时器（见 `on_pet_click` / `on_pet_tick`）。
pub(super) unsafe extern "system" fn bar_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_APP_REFRESH => {
            // FIFO 有新消息：drain 串行执行显隐动作。
            if let Some(w) = get_bar_mut(hwnd) {
                w.reconcile();
            }
            LRESULT(0)
        }
        WM_TIMER => {
            // M1 桌宠：动画 tick（30fps 上限；空闲停帧已 sync_pet_timer KillTimer）。
            if wparam.0 == PET_TIMER_ID {
                if let Some(w) = get_bar_mut(hwnd) {
                    w.on_pet_tick();
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            // ULW 内容不画窗口 DC：BeginPaint/EndPaint 成对校验更新区（防风暴）。
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !hdc.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_HOTKEY => {
            // 全局热键触发（41-keymap-settings.md §4）：wParam 低 16 位 = 热键 id。
            let id = (wparam.0 & 0xFFFF) as usize;
            if let Some((action, _secondary)) = crate::hotkey::hotkey_from_id(id) {
                if let Some(w) = get_bar_mut(hwnd) {
                    w.on_hotkey(action);
                }
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // 悬停光标：客户区按按钮命中二选一——功能钮（四态/齿轮）= 手指头，
            // logo/空白/宠物区透明 = 箭头；非客户区走类默认（箭头）。lparam 不含坐标，取
            // GetCursorPos − 窗口原点得客户区坐标（同 WM_NCHITTEST 臂手法）。
            // 拖拽捕获期间系统不发本消息，无需特判。
            if (lparam.0 as u32 & 0xFFFF) == HTCLIENT as u32 {
                let cursor_kind = get_bar_mut(hwnd).map(|w| {
                    let (sx, sy) = cursor_screen();
                    let mut rc = RECT::default();
                    // SAFETY: GetWindowRect/GetCursorPos 纯查询。
                    if unsafe { GetWindowRect(hwnd, &mut rc) }.is_err() {
                        return IDC_ARROW;
                    }
                    let (cx, cy) = (sx - rc.left, sy - rc.top);
                    // M1 桌宠：宠物像素点 → 手型（P2 提案，纳入"功能位"判定族）
                    if w.pet_pixel_hit(cx, cy) {
                        return IDC_HAND;
                    }
                    // 工具栏按钮命中（含 logo 排除，logo 走箭头）
                    let over_button = hit_test(&w.rows, cx, cy)
                        .is_some_and(|i| i != TB_LOGO);
                    if over_button {
                        IDC_HAND
                    } else {
                        IDC_ARROW
                    }
                });
                // SAFETY: SetCursor 设标准内置光标；LoadCursorW 取系统 stock 光标。
                unsafe {
                    let cursor = LoadCursorW(None, cursor_kind.unwrap_or(IDC_ARROW))
                        .unwrap_or_default();
                    SetCursor(Some(cursor));
                }
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_NCHITTEST => {
            // M1 桌宠：复合窗口命中判定顺序（§5.2）：
            //   1) 宠物像素命中（alpha > 阈值） → HTCLIENT（可点可拖）
            //   2) 工具栏背景圆角矩形内 → HTCLIENT（按钮命中）
            //   3) 其余（宠物区透明、工具栏圆角外、工具栏右侧空隙） → HTTRANSPARENT
            let (sx, sy) = client_pos(lparam); // 屏幕坐标
            let mut rc = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut rc) }.is_err() {
                return LRESULT(HTCLIENT as isize);
            }
            let (x, y) = (sx - rc.left, sy - rc.top);
            // 1) 宠物像素命中（在判定工具栏之前：宠物区在工具栏之上，水平方向无重叠）
            if get_bar_mut(hwnd)
                .map(|wnd| wnd.pet_pixel_hit(x, y))
                .unwrap_or(false)
            {
                return LRESULT(HTCLIENT as isize);
            }
            // 2) 工具栏背景圆角矩形内（y 偏移 PET_OVERHANG，复用 frame 缓存几何）
            let (tx, ty, tw, th, radius) = match get_bar_mut(hwnd) {
                Some(wnd) => (x, y - wnd.overhang, wnd.toolbar_w, wnd.toolbar_h, wnd.radius),
                None => return LRESULT(HTTRANSPARENT as isize),
            };
            if ty >= 0 && ty < th && tx >= 0 && tx < tw
                && in_rounded_rect(tx, ty, tw, th, radius)
            {
                return LRESULT(HTCLIENT as isize);
            }
            // 3) 其余穿透
            LRESULT(HTTRANSPARENT as isize)
        }
        WM_MOUSEMOVE => {
            let (x, y) = client_pos(lparam);
            if let Some(w) = get_bar_mut(hwnd) {
                // 拖拽中：按屏幕坐标移动窗口；否则：悬停更新。
                if w.drag_offset.is_some() {
                    w.drag_move();
                } else {
                    // TrackMouseEvent 重挂 WM_MOUSELEAVE（离开窗口清悬停）。
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        ..Default::default()
                    };
                    let _ = unsafe { TrackMouseEvent(&mut tme) };
                    w.on_hover(x, y);
                }
                // 宠物按下判别：记录下笔点，超过阈值 → 升格为拖拽（与工具栏拖拽共用管线）
                if let Some((press_screen, _pet_rect)) = w.pet_down {
                    let (cx, cy) = cursor_screen();
                    let (dx, dy) = (cx - press_screen.0, cy - press_screen.1);
                    if dx * dx + dy * dy > PET_DRAG_THRESHOLD * PET_DRAG_THRESHOLD {
                        // 升格为整窗拖拽：复用现有 start_drag 语义（§5.3「start_drag_at(按下点)」）
                        // ——drag_offset = 光标屏坐标 - 窗口位置，保证拖拽跟随光标不跳窗
                        // （旧实现 offset=(0,0) 会把窗口左上角瞬移到光标处，宠物偏离栖木位）。
                        let _ = w.pet_down.take();
                        w.start_drag();
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(w) = get_bar_mut(hwnd) {
                w.hover = None;
                w.tip.hide();
                w.repaint();
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = client_pos(lparam);
            if let Some(w) = get_bar_mut(hwnd) {
                // M1 桌宠：先看宠物像素命中（在按钮命中之前：宠物在工具栏上沿之上，不重叠）
                if w.pet_pixel_hit(x, y) {
                    if let Some(pr) = w.pet_rect {
                        let (cx, cy) = cursor_screen();
                        w.pet_down = Some(((cx, cy), pr));
                        // SetCapture：保证用户拖到窗外 / 在窗内释放时 WM_LBUTTONUP 仍能收到；
                        // click vs drag 判别靠"是否移动 > 阈值"，与 SetCapture 无冲突（传统模式）。
                        // SAFETY: SetCapture 捕获鼠标到本窗口（拖拽期间持续收 WM_MOUSEMOVE）。
                        let _ = unsafe { SetCapture(hwnd) };
                    }
                    LRESULT(0)
                } else {
                    match hit_test(&w.rows, x, y) {
                        // 空白区或 logo（拖动把手）：开始拖拽（§6.6 任意非按钮空白区拖动）。
                        None | Some(TB_LOGO) => {
                            w.start_drag();
                        }
                        Some(i) => {
                            // 功能按钮按下（点击反馈）+ 鼠标抬起时执行。
                            w.pressed = Some(i);
                            w.repaint();
                        }
                    }
                    LRESULT(0)
                }
            } else {
                LRESULT(0)
            }
        }
        WM_LBUTTONUP => {
            if let Some(w) = get_bar_mut(hwnd) {
                // M1 桌宠：先看 pet_down（按下宠物后的抬起事件）
                if w.pet_down.is_some() {
                    let _ = w.pet_down.take();
                    // 释放捕获（若有）
                    if w.drag_offset.is_none() {
                        let _ = unsafe { ReleaseCapture() };
                    }
                    // 未升格为拖拽 → 触发宠物点击互动
                    if w.drag_offset.is_none() {
                        w.on_pet_click();
                    } else {
                        // 拖拽中：复用现有 end_drag
                        w.end_drag();
                    }
                    return LRESULT(0);
                }
                if let Some(i) = w.pressed.take() {
                    w.repaint();
                    // 释放捕获（若有）
                    if w.drag_offset.is_none() {
                        let _ = unsafe { ReleaseCapture() };
                    }
                    // 点击按钮（抬起执行，贴合 Windows 惯例）
                    w.on_click(i);
                }
                if w.drag_offset.is_some() {
                    w.end_drag();
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // 工具条线程退出清理触发（Drop 销毁窗口）；消息循环以 WM_QUIT 结束。
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}