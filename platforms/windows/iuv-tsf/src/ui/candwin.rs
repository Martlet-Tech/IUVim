//! UpdateLayeredWindow 候选窗实现（M4 起，2026-08-17 定稿为 ULW 路线）。
//! 契约 01-contract.md §5、任务书 14-mod-iuv-tsf-candwin.md、19-m4-cross-render.md §3。
//! 无边框 / 置顶 / **不抢焦点**（`WS_EX_NOACTIVATE` + `SW_SHOWNA`，绝不 `SetForegroundWindow`/`SetFocus`）；
//! 呈现 = iuv-ui（tiny-skia + cosmic-text）软件光栅 premultiplied BGRA →
//! `WS_EX_LAYERED` + `UpdateLayeredWindow`（ULW_ALPHA）交 DWM per-pixel 合成
//! （真透明圆角/阴影）。**无任何 GPU 设备依赖**（D3D11/D2D/DComp 路线已弃——
//! DComp 关联 D2D 的 BeginDraw 恒 E_INVALIDARG，且 D2D↔DComp surface 互操作
//! CreateBitmapFromDxgiSurface 亦 E_INVALIDARG，2026-08-17 实测，见 19-m4 §5）。
//! 全部对外方法不返回错误：任何失败静默降级（隐藏窗口 / 不显示），**绝不 panic**。
//!
//! 渲染管线（每帧 show/update/悬停重绘统一走 `present`）：
//! 1. `scale = dpi/96`（LOGPIXELSY 路径，同 gdi.rs）
//! 2. `iuv_ui::render_candidate(&snap, &theme, scale, &mut text)` → `Surface`
//!    （premultiplied BGRA，尺寸含阴影外缘）
//! 3. 尺寸变化 → 重建 DIB section（32bpp 自顶向下，内存序 BGRA 与 Surface 一致）
//! 4. `UpdateLayeredWindow(hwnd, dst=桌面, &pt{窗口位置}, &size, hdc_src(DIB),
//!    &pt{0,0}, 0, &blend{AC_SRC_OVER, 255, AC_SRC_ALPHA}, ULW_ALPHA)`
//!    ——一次调用同时定位 + 定尺寸 + 上屏；DWM per-pixel alpha 合成。
//!
//! 已知限制（M4 接受，见任务书 §5 槽位）：
//! - 主题在装配时注入（config 读一次）；config 热改深色切换不做（M6 设置页做重载）。
//! - DPI 变化不监听（冻结 feature 集无 HiDpi）：每帧按 LOGPIXELSY 现取。
//! - 窗口必须由调用线程创建/销毁（TSF 回调线程有消息循环，成立）；`Drop` 需在同一线程。
//! - Layered 窗口全量重传（~300×200 每键一次，性能无虞）；无效果器（阴影由 iuv-ui 软件画）。

use std::mem::size_of;
use std::sync::OnceLock;

use super::{CandidateUi, CaretRect, UiSnapshot};
use iuv_ui::layout::{Area, Rect};
use iuv_ui::{hit_test, render_candidate, update_position, TextRenderer, Theme, FONT_PX_96};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowRect,
    RegisterClassExW, SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HTCLIENT, HTTRANSPARENT, MA_NOACTIVATE,
    SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_RBUTTONDOWN, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS_NAME: PCWSTR = w!("IuvCandidateWindow");

/// 主字号（px @96dpi）；dpi 缩放由每帧 scale 处理。
const FONT_PX: f32 = FONT_PX_96;

/// ULW 自绘候选窗：无边框、置顶、不抢焦点、真透明圆角/阴影。
/// `new(theme)` 不建窗；首次 `show` 懒建（窗口必须建在调用线程）。
pub struct CandwinCandidateWindow {
    hwnd: HWND,
    snap: UiSnapshot,
    visible: bool,
    /// 最近一次定位用的光标锚点（update 超屏时翻屏用）。
    last_caret: Option<CaretRect>,
    /// 最近一次布局的候选矩形列表（横竖统一，命中测试用）。
    rows: Vec<Rect>,
    /// 点击候选回调（行号 0 起；None = 未接线）。
    click: Option<Box<dyn Fn(usize)>>,
    /// 悬停候选回调（行号 0 起，行变化才触发；None = 未接线）。
    hover: Option<Box<dyn Fn(usize)>>,
    /// 抑制显示：IMM 应用（游戏自绘候选栏）时静默（show/update 空操作）。
    suppressed: bool,
    /// 主题（装配时从 config 注入；M6 起可经 `set_theme` 热载——设置页保存后
    /// config_epoch 变化 → text_service 调 set_theme，下帧 paint 用新主题重渲染）。
    theme: Theme,
    /// 文本渲染器（fontdb 首扫只在窗口创建时一次；每帧 measure+draw 复用）。
    text: Option<TextRenderer>,
    /// ULW 呈现缓存（懒建；尺寸变化自动重建 DIB）。
    ulw: super::ulw::UlwSurface,
}

impl CandwinCandidateWindow {
    /// 以指定主题构造（不建窗，首次 show 懒建）。
    pub fn new(theme: Theme) -> Self {
        CandwinCandidateWindow {
            hwnd: HWND::default(),
            snap: UiSnapshot::default(),
            visible: false,
            last_caret: None,
            rows: Vec::new(),
            click: None,
            hover: None,
            suppressed: false,
            theme,
            text: None,
            ulw: super::ulw::UlwSurface::new(),
        }
    }

    /// 接线点击回调（text_service 构造时注入；同线程调用）。
    pub fn set_on_click(&mut self, cb: Option<Box<dyn Fn(usize)>>) {
        self.click = cb;
    }

    /// 接线悬停回调（text_service 构造时注入；同线程调用）。
    pub fn set_on_hover(&mut self, cb: Option<Box<dyn Fn(usize)>>) {
        self.hover = cb;
    }

    /// M6 热载主题（config_epoch 变化 → 设置页保存后触发）：存字段 + 原位重绘
    /// （即时切换，无需重载输入法）。
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.repaint();
    }

    /// 进程内注册一次窗口类；失败（非"已注册"）记日志，不 panic。
    fn register_class() {
        static REGISTERED: OnceLock<()> = OnceLock::new();
        REGISTERED.get_or_init(|| {
            // SAFETY: 所有字段显式/Default 填充；类名为静态宽字符串，进程生命周期有效。
            // 失败仅记日志（W0 桩 log.rs，Agent D 会落盘 %TEMP%\iuv-tsf.log）。
            unsafe {
                let class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(wnd_proc),
                    hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
                    lpszMenuName: PCWSTR::null(),
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                if RegisterClassExW(&class) == 0 {
                    let err = GetLastError();
                    if err != ERROR_CLASS_ALREADY_EXISTS {
                        crate::log::log_line("[candwin] RegisterClassExW 失败");
                    }
                }
            }
        });
    }

    /// 当前 DPI：窗口 HDC 的 `LOGPIXELSY`，失败兜底 96。
    fn get_dpi(&self) -> u32 {
        // SAFETY: hdc 由 GetDC 取得，使用后立即 ReleaseDC
        unsafe {
            let hdc = windows::Win32::Graphics::Gdi::GetDC(Some(self.hwnd));
            if hdc.is_invalid() {
                return 96;
            }
            let dpi = windows::Win32::Graphics::Gdi::GetDeviceCaps(
                Some(hdc),
                windows::Win32::Graphics::Gdi::LOGPIXELSY,
            );
            windows::Win32::Graphics::Gdi::ReleaseDC(Some(self.hwnd), hdc);
            if dpi <= 0 {
                96
            } else {
                dpi as u32
            }
        }
    }

    /// 懒建窗口 + 文本渲染器（ULW 呈现缓存随首次 present 建立，无设备初始化）。
    /// 失败仅记日志并保持 `hwnd` 无效（后续调用静默降级）。
    fn ensure_window(&mut self) {
        if !self.hwnd.is_invalid() {
            return;
        }
        Self::register_class();
        // SAFETY: GetModuleHandleW(None) 取当前进程实例句柄
        let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        if hinst.is_invalid() {
            crate::log::log_line("[candwin] GetModuleHandleW 失败");
            return;
        }
        // SAFETY: WS_EX_TOPMOST|TOOLWINDOW|NOACTIVATE 保证置顶且不抢焦点；
        // WS_EX_LAYERED = per-pixel alpha 合成（UpdateLayeredWindow 前置条件）；
        // WS_POPUP 无边框。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                CLASS_NAME,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinst.into()),
                None,
            )
        };
        let Ok(hwnd) = hwnd else {
            crate::log::log_line("[candwin] CreateWindowExW 失败");
            return;
        };
        // SAFETY: self 仅在创建线程存活；Drop 先清零 GWLP_USERDATA 再销毁窗口，
        // 因此 wnd_proc 经 GetWindowLongPtrW 取到的指针不会悬垂。
        // `as usize as _` 按平台推断：x64 = isize（指针同宽），x86 = i32（32 位指针无损）。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, self as *mut Self as usize as _) };
        self.hwnd = hwnd;

        // TextRenderer：fontdb 首扫系统字体（几十 ms，仅窗口创建时一次，可接受）。
        self.text = Some(TextRenderer::new());
    }

    /// 渲染当前 snapshot → Surface（含阴影外缘尺寸；失败返回 None）。
    /// 同时刷新命中测试行矩形（与 render_candidate 内部 layout 同测量，保证一致）。
    fn frame(&mut self) -> Option<iuv_ui::Surface> {
        if self.snap.reading.is_empty() && self.snap.candidates.is_empty() {
            return None;
        }
        let scale = self.get_dpi() as f32 / 96.0;
        let surf = {
            let text = self.text.as_mut()?;
            render_candidate(&self.snap, &self.theme, scale, text)
        };
        if surf.w == 0 || surf.h == 0 {
            return None;
        }
        self.rows = self.compute_rows(scale);
        Some(surf)
    }

    /// 命中测试行矩形：与 render_candidate 相同的测量规则（候选编号/页码/字号）。
    fn compute_rows(&mut self, scale: f32) -> Vec<Rect> {
        let labels: Vec<String> = self
            .snap
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| self.candidate_label(i, c))
            .collect();
        let Some(text) = self.text.as_mut() else {
            return Vec::new();
        };
        let size_px = FONT_PX * scale;
        let page_px = size_px / 2.0;
        let mut sizes: Vec<(String, (i32, i32))> = Vec::with_capacity(labels.len());
        for (label, _cand) in labels.iter().zip(self.snap.candidates.iter()) {
            sizes.push((label.clone(), text.measure(label, size_px)));
        }
        let mut page_size = None;
        if self.snap.page.page_count > 1 {
            let label = format!("{}/{}", self.snap.page.page + 1, self.snap.page.page_count);
            page_size = Some(text.measure(&label, page_px));
        }
        let (_, _, rects) = iuv_ui::layout(
            &self.snap,
            |s| {
                sizes
                    .iter()
                    .find(|(t, _)| t == s)
                    .map(|(_, sz)| *sz)
                    .unwrap_or((0, 0))
            },
            |_s| page_size.unwrap_or((0, 0)),
            self.snap.orientation,
        );
        rects
    }

    /// 候选行显示文本：原文兜底候选不编号（与 iuv-ui candidate_label 规则一致）。
    fn candidate_label(&self, index: usize, cand: &str) -> String {
        if cand == self.snap.reading.replace('\'', "") {
            cand.to_string()
        } else {
            format!("{}.{}", index + 1, cand)
        }
    }

    /// 按当前 snapshot 重算尺寸 → 定位（caret 或原位）→ ULW 上屏
    /// （一次调用同时定位 ptDst + 定尺寸 psize + per-pixel alpha 合成）。
    fn apply_layout_and_pos(&mut self, caret: Option<CaretRect>) {
        let Some(surf) = self.frame() else {
            return;
        };
        let w = surf.w as i32;
        let h = surf.h as i32;
        let (x, y) = match caret {
            Some(c) => position_for(c, w, h),
            None => {
                // SAFETY: GetWindowRect 读当前窗口矩形
                let mut rc = RECT::default();
                let _ = unsafe { GetWindowRect(self.hwnd, &mut rc) };
                update_position(
                    (rc.left, rc.top),
                    w,
                    h,
                    work_area_for(self.hwnd),
                    self.last_caret,
                )
            }
        };
        self.present(&surf, x, y, w, h);
    }

    /// 原位重绘（悬停高亮 / 主题热载）：读当前窗口矩形 → 渲染 → ULW 上屏。
    fn repaint(&mut self) {
        if self.hwnd.is_invalid() || !self.visible {
            return;
        }
        let Some(surf) = self.frame() else {
            return;
        };
        // SAFETY: GetWindowRect 读当前窗口矩形
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_err() {
            return;
        }
        self.present(
            &surf,
            rc.left,
            rc.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
        );
    }

    /// ULW 呈现：确保 DIB（尺寸变化重建）→ 拷贝 premultiplied BGRA → 上屏。
    /// 失败记日志，静默降级（不 panic）。
    /// ULW 呈现（共享模块 ulw.rs：DIB 重建 + 像素直拷 + UpdateLayeredWindow
    /// 一次定位/定尺寸/per-pixel alpha 合成）。失败静默（记日志，不 panic）。
    fn present(&mut self, surf: &iuv_ui::Surface, x: i32, y: i32, w: i32, h: i32) {
        self.ulw.upload(self.hwnd, surf, x, y, w, h, "[candwin]");
    }
}

/// 窗口所在显示器的物理工作区；失败兜底近乎全屏区域。
fn work_area_for(hwnd: HWND) -> Area {
    // SAFETY: MonitorFromWindow 纯查询；GetMonitorInfoW 输出缓冲已初始化。
    let monitor = unsafe {
        windows::Win32::Graphics::Gdi::MonitorFromWindow(
            hwnd,
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = windows::Win32::Graphics::Gdi::MONITORINFO {
        cbSize: size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { windows::Win32::Graphics::Gdi::GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        rect_to_area(info.rcWork)
    } else {
        Area {
            left: 0,
            top: 0,
            right: 32767,
            bottom: 32767,
        }
    }
}

/// 按 caret 定位：优先取光标所在显示器的物理工作区（GetTextExt 返回物理像素坐标，
/// SPI_GETWORKAREA 只返回主屏工作区——副屏打字时会把候选框 clamp 回主屏）。
fn position_for(caret: CaretRect, w: i32, h: i32) -> (i32, i32) {
    // SAFETY: MonitorFromPoint 纯查询，无资源；caret 为本地值。
    let monitor = unsafe {
        windows::Win32::Graphics::Gdi::MonitorFromPoint(
            POINT {
                x: caret.x,
                y: caret.y,
            },
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = windows::Win32::Graphics::Gdi::MONITORINFO {
        cbSize: size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: GetMonitorInfoW 输出缓冲 info 在调用前初始化且存活。
    let area = if unsafe { windows::Win32::Graphics::Gdi::GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
    {
        rect_to_area(info.rcWork)
    } else {
        // 兜底：主屏工作区；失败时近乎全屏区域。
        // SAFETY: SPI_GETWORKAREA 需要可写 RECT。
        let mut rc = RECT::default();
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut rc as *mut RECT as *mut core::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        if ok.is_err() {
            Area {
                left: 0,
                top: 0,
                right: 32767,
                bottom: 32767,
            }
        } else {
            rect_to_area(rc)
        }
    };
    iuv_ui::position_in_area(caret, w, h, area)
}

fn rect_to_area(rc: RECT) -> Area {
    Area {
        left: rc.left,
        top: rc.top,
        right: rc.right,
        bottom: rc.bottom,
    }
}

impl Default for CandwinCandidateWindow {
    fn default() -> Self {
        Self::new(iuv_ui::theme_light())
    }
}

impl CandidateUi for CandwinCandidateWindow {
    fn show(&mut self, snap: &UiSnapshot, caret: CaretRect) {
        if self.suppressed {
            return; // IMM 应用：游戏自绘候选栏，本窗静默
        }
        if snap.reading.is_empty() && snap.candidates.is_empty() {
            crate::log::log_line("[candwin] show：快照为空，转 hide");
            self.hide();
            return;
        }
        self.snap = snap.clone();
        self.last_caret = Some(caret);
        if self.hwnd.is_invalid() {
            self.ensure_window();
            if self.hwnd.is_invalid() {
                return; // 建窗失败：静默降级（绝不 panic）
            }
        }
        self.apply_layout_and_pos(Some(caret));
        // SAFETY: SW_SHOWNA 显示但不激活——绝不抢焦点
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNA) };
        self.visible = true;
    }

    fn update(&mut self, snap: &UiSnapshot) {
        if self.suppressed {
            return; // IMM 应用：本窗静默
        }
        self.snap = snap.clone();
        if self.hwnd.is_invalid() || !self.visible {
            return;
        }
        self.apply_layout_and_pos(None);
    }

    fn move_to(&mut self, caret: CaretRect) {
        if self.hwnd.is_invalid() || !self.visible {
            return;
        }
        self.last_caret = Some(caret);
        // SAFETY: GetWindowRect 读当前窗口矩形
        unsafe {
            let mut rc = RECT::default();
            if GetWindowRect(self.hwnd, &mut rc).is_err() {
                return;
            }
            let w = rc.right - rc.left;
            let h = rc.bottom - rc.top;
            let (x, y) = position_for(caret, w, h);
            // SAFETY: 仅移动（SWP_NOSIZE），不激活；layered 窗口内容由 DWM 缓存随动
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE,
            );
        }
    }

    fn hide(&mut self) {
        self.visible = false;
        if !self.hwnd.is_invalid() {
            // SAFETY: 隐藏候选窗
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_suppressed(&mut self, suppressed: bool) {
        if self.suppressed == suppressed {
            return;
        }
        self.suppressed = suppressed;
        if suppressed {
            // 抑制开启：立即隐藏已显示的窗口（游戏自绘候选栏接管）。
            self.hide();
        }
    }
}

impl Drop for CandwinCandidateWindow {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: 先清零 GWLP_USERDATA，杜绝 wnd_proc 访问到即将释放的 self
            let _ = unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            // SAFETY: 在创建线程上销毁窗口
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            };
            self.hwnd = HWND::default();
        }
        // ulw（DIB/DC）与 text 按字段声明序自然 drop。
    }
}

/// lparam 低 32 位的坐标 (x, y)（WM_MOUSEMOVE 等 = 客户区；WM_NCHITTEST = 屏幕坐标）。
fn client_pos(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as u32;
    ((v & 0xFFFF) as i32, ((v >> 16) & 0xFFFF) as i32)
}

/// 圆角几何命中：窗口矩形内、圆角弧线内（含边界）→ true（HTCLIENT）；
/// 四角圆弧外 → false（HTTRANSPARENT，点击穿透到下层窗口）。
/// 半径按当前主题 corner_radius × DPI scale，与渲染像素一致。
fn in_rounded_rect(x: i32, y: i32, w: i32, h: i32, r: f32) -> bool {
    if w <= 0 || h <= 0 {
        return false;
    }
    if !r.is_finite() || r <= 0.0 {
        return true;
    }
    // 半径钳制到 min(w, h) / 2（与 tiny-skia 圆角路径一致，避免 w - r 越界）
    let r = (r as i32).min(w / 2).min(h / 2).max(1);
    let in_corner = |cx: i32, cy: i32, px: i32, py: i32| {
        let dx = px - cx;
        let dy = py - cy;
        dx * dx + dy * dy <= r * r
    };
    if x < r && y < r {
        return in_corner(r, r, x, y);
    }
    if x >= w - r && y < r {
        return in_corner(w - r, r, x, y);
    }
    if x < r && y >= h - r {
        return in_corner(r, h - r, x, y);
    }
    if x >= w - r && y >= h - r {
        return in_corner(w - r, h - r, x, y);
    }
    true
}

/// 从 GWLP_USERDATA 取回窗口属主；0（未挂接/已销毁）返回 None。
fn get_self(hwnd: HWND) -> Option<&'static CandwinCandidateWindow> {
    // SAFETY: 指针在窗口销毁前由 Drop 先清零，不悬垂；调用都在创建线程
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        // SAFETY: p 非 0 即此前 SetWindowLongPtrW 写入的有效指针
        Some(unsafe { &*(p as *const CandwinCandidateWindow) })
    }
}

/// 可变版（hover 更新本地 selected 高亮用）。线程约束同 `get_self`。
fn get_self_mut(hwnd: HWND) -> Option<&'static mut CandwinCandidateWindow> {
    // SAFETY: 同 get_self：指针生命周期由 Drop 清零保证；调用都在创建线程
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        // SAFETY: p 非 0 即此前 SetWindowLongPtrW 写入的有效指针
        Some(unsafe { &mut *(p as *mut CandwinCandidateWindow) })
    }
}

/// 类窗口过程：WM_PAINT 只做 BeginPaint/EndPaint 校验区管理（ULW 内容不画窗口 DC，
/// 每帧内容由 UpdateLayeredWindow 直接送 DWM 合成）；
/// WM_ERASEBKGND 返回 1（layered 窗口无 GDI 背景可擦）；
/// WM_NCHITTEST 按圆角几何判定（圆角外 HTTRANSPARENT 点击穿透）；
/// 鼠标交互：WM_MOUSEACTIVATE 显式 MA_NOACTIVATE（点击候选窗绝不改变激活——
/// 无 owner popup 的激活转移会让宿主（WinUI3 记事本实测）失活崩 TSF，2026-08-13）；
/// WM_MOUSEMOVE 悬停命中 → 本地高亮 + 回调同步会话 + ULW 原位重绘；
/// WM_LBUTTONDOWN 命中 → 点击回调选词上屏；其余鼠标消息一律吞掉
/// （不回 DefWindowProc，杜绝默认行为）。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: BeginPaint 仅允许在 WM_PAINT 内调用；hwnd 由消息循环保证有效。
            // ULW 呈现不画进窗口 DC，但必须成对调用以校验更新区（防 WM_PAINT 风暴）。
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !hdc.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            let (sx, sy) = client_pos(lparam); // 屏幕坐标
            let mut rc = RECT::default();
            if GetWindowRect(hwnd, &mut rc).is_err() {
                return LRESULT(HTCLIENT as isize);
            }
            let (x, y) = (sx - rc.left, sy - rc.top);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let radius = match get_self(hwnd) {
                Some(wnd) => wnd.theme.corner_radius * wnd.get_dpi() as f32 / 96.0,
                None => 0.0,
            };
            if in_rounded_rect(x, y, w, h, radius) {
                LRESULT(HTCLIENT as isize)
            } else {
                // 圆角外（窗口四角透明区）：点击穿透到下层窗口
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = client_pos(lparam);
            if let Some(wnd) = get_self_mut(hwnd) {
                if let Some(row) = hit_test(&wnd.rows, x, y) {
                    if row < wnd.snap.candidates.len() && row != wnd.snap.selected {
                        wnd.snap.selected = row;
                        if let Some(cb) = wnd.hover.as_ref() {
                            cb(row);
                        }
                        // SAFETY: 悬停行变化 → ULW 原位重绘高亮（无 WM_PAINT 依赖）
                        wnd.repaint();
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = client_pos(lparam);
            if let Some(wnd) = get_self(hwnd) {
                if let Some(row) = hit_test(&wnd.rows, x, y) {
                    if row < wnd.snap.candidates.len() {
                        if let Some(cb) = wnd.click.as_ref() {
                            cb(row);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_MBUTTONDOWN => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_rounded_rect_center_hit() {
        assert!(in_rounded_rect(50, 50, 200, 100, 8.0), "中心必然命中");
        assert!(in_rounded_rect(0, 0, 200, 100, 0.0), "半径 0 = 全矩形命中");
    }

    #[test]
    fn in_rounded_rect_corners_transparent() {
        // 圆角 r=8：角尖 (1,1) 在圆弧外（点击穿透）
        assert!(!in_rounded_rect(1, 1, 200, 100, 8.0), "左上角圆弧外");
        assert!(!in_rounded_rect(199, 1, 200, 100, 8.0), "右上角圆弧外");
        assert!(!in_rounded_rect(1, 99, 200, 100, 8.0), "左下角圆弧外");
        assert!(!in_rounded_rect(199, 99, 200, 100, 8.0), "右下角圆弧外");
        // 圆弧上（对角线上距圆心 8px 处）命中
        assert!(in_rounded_rect(8, 8, 200, 100, 8.0), "圆弧边界命中");
        assert!(in_rounded_rect(8, 50, 200, 100, 8.0), "左边缘中点命中");
        assert!(in_rounded_rect(100, 99, 200, 100, 8.0), "下边缘中点命中");
    }

    #[test]
    fn in_rounded_rect_radius_clamped() {
        // 半径大于 min(w,h)/2：钳制到 5（不越界、不 panic）；判定按钳制后几何。
        assert!(
            !in_rounded_rect(1, 1, 10, 10, 100.0),
            "钳制后圆角 (5,5) r=5：角尖 (1,1) 距圆心 √32>5 → 圆弧外"
        );
        assert!(
            in_rounded_rect(3, 3, 10, 10, 100.0),
            "钳制后圆角内点 (3,3) 距圆心 √8<5 → 命中"
        );
        assert!(
            !in_rounded_rect(0, 3, 10, 10, 100.0),
            "左上圆角外 (0,3)：距圆心 (5,5) √29>5 → 穿透"
        );
        assert!(
            in_rounded_rect(0, 5, 10, 10, 100.0),
            "左缘中点 (0,5) 在竖直边上（非圆角区）→ 命中"
        );
    }

    #[test]
    fn candidate_label_rules() {
        // 原文兜底候选不编号；正常候选编号（与 iuv-ui candidate_label 规则一致）
        let mut w = CandwinCandidateWindow::new(iuv_ui::theme_light());
        w.snap = UiSnapshot {
            reading: "i'n'pu't".into(),
            ..Default::default()
        };
        assert_eq!(w.candidate_label(0, "input"), "input", "原文兜底不编号");
        w.snap.reading = "ni'hao".into();
        assert_eq!(w.candidate_label(0, "你好"), "1.你好", "正常候选编号");
    }
}
