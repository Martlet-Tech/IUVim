//! D2D 1.1 + DirectComposition 候选窗实现（M4 起，替代 gdi.rs）。
//! 契约 01-contract.md §5、任务书 14-mod-iuv-tsf-candwin.md、19-m4-cross-render.md §3。
//! 无边框 / 置顶 / **不抢焦点**（`WS_EX_NOACTIVATE` + `SW_SHOWNA`，绝不 `SetForegroundWindow`/`SetFocus`）；
//! 呈现 = iuv-ui（tiny-skia + cosmic-text）软件光栅 → D2D 1.1 DeviceContext 直供
//! DirectComposition surface → DWM per-pixel 透明合成（真透明圆角/阴影）；
//! 全部对外方法不返回错误：任何失败静默降级（隐藏窗口 / 不显示），**绝不 panic**。
//!
//! 渲染管线（每帧 show/update/WM_PAINT 统一走 `paint`）：
//! 1. `scale = dpi/96`（LOGPIXELSY 路径，同 gdi.rs）
//! 2. `iuv_ui::render_candidate(&snap, &theme, scale, &mut text)` → `Surface`
//!    （premultiplied BGRA，尺寸含阴影外缘）
//! 3. surface 尺寸变化 → `IDCompositionDevice::CreateSurface(w, h,
//!    DXGI_FORMAT_B8G8R8A8_UNORM_PREMULTIPLIED, DXGI_ALPHA_MODE_PREMULTIPLIED)`，
//!    `visual->SetContent(surface)`
//! 4. `surface->BeginDraw` → `ID2D1DeviceContext::CreateBitmap`（内存像素直供，1:1）
//!    → `DrawBitmap` → 释放 ctx → `surface->EndDraw()` → `device->Commit()`
//!
//! 初始化失败路径：D3D11 硬件设备失败 → WARP 软件兜底 → 再失败 → `degraded`
//! （候选窗永不显示，输入法主体不受影响）。全部失败记日志，绝不 panic。
//!
//! 已知限制（M4 接受，见任务书 §5 槽位）：
//! - 主题在装配时注入（config 读一次）；config 热改深色切换不做（M6 设置页做重载）。
//! - DPI 变化不监听（冻结 feature 集无 HiDpi）：每帧按 LOGPIXELSY 现取。
//! - 窗口必须由调用线程创建/销毁（TSF 回调线程有消息循环，成立）；`Drop` 需在同一线程。

use std::mem::size_of;
use std::sync::OnceLock;

use super::{CandidateUi, CaretRect, UiSnapshot};
use iuv_ui::layout::{Area, Rect};
use iuv_ui::{hit_test, render_candidate, update_position, TextRenderer, Theme, FONT_PX_96};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateDevice, ID2D1Device, ID2D1DeviceContext, D2D1_BITMAP_PROPERTIES1,
    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionSurface, IDCompositionTarget,
    IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowRect,
    RegisterClassExW, SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HTCLIENT, HTTRANSPARENT, MA_NOACTIVATE, SPI_GETWORKAREA,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST,
    WM_PAINT, WM_RBUTTONDOWN, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows_core::Interface;

const CLASS_NAME: PCWSTR = w!("IuvCandidateWindow");

/// 主字号（px @96dpi）；dpi 缩放由每帧 scale 处理。
const FONT_PX: f32 = FONT_PX_96;

/// D2D/DComp 渲染资源（与窗口同生命周期）。
/// 字段声明序 = 释放逆序（surface → visual → target → dcomp device → d2d → d3d）；
/// windows-rs 对象 drop 即 COM 引用计数释放，顺序仅为整洁约束。
struct RenderState {
    /// 合成 surface（首次上屏时创建；尺寸变化时重建 + 重挂 SetContent）。
    surface: Option<IDCompositionSurface>,
    visual: IDCompositionVisual,
    /// 目标/设备句柄：初始化后只做持有（COM 引用计数保证 DWM 侧引用有效），不直接读写。
    #[allow(dead_code)]
    target: IDCompositionTarget,
    device: IDCompositionDevice,
    /// 设备句柄：只做持有（每帧 DeviceContext 由 BeginDraw 产出，无需预建）。
    #[allow(dead_code)]
    d2d: ID2D1Device,
    /// 设备句柄：只做持有（D3D 设备生命周期必须长于 D2D/DComp）。
    #[allow(dead_code)]
    d3d: ID3D11Device,
    /// 当前 surface 尺寸（变化时重建 surface + 重挂 SetContent）。
    surface_w: u32,
    surface_h: u32,
}

/// D2D/DComp 自绘候选窗：无边框、置顶、不抢焦点、真透明圆角/阴影。
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
    /// D2D/DComp 资源（窗口创建时初始化；失败 = degraded，候选窗永不显示）。
    render: Option<RenderState>,
    /// 渲染资源创建失败：show/update 静默（维持"绝不 panic"哲学，输入法不受影响）。
    degraded: bool,
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
            render: None,
            degraded: false,
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

    /// M6 热载主题（config_epoch 变化 → 设置页保存后触发）：存字段，下帧 paint 生效；
    /// 窗口已建 → 触发重绘（即时切换，无需重载输入法）。
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        if !self.hwnd.is_invalid() {
            // SAFETY: InvalidateRect/UpdateWindow 触发 WM_PAINT（paint 用新主题全量重渲染）。
            let _ = unsafe {
                windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false)
            };
            let _ = unsafe { windows::Win32::Graphics::Gdi::UpdateWindow(self.hwnd) };
        }
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

    /// 懒建窗口 + 渲染资源（D3D11 → D2D1.1 → DComp + TextRenderer）。
    /// 失败仅记日志并保持 `hwnd` 无效 / `degraded`（后续调用静默降级）。
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
        // SAFETY: WS_EX_TOPMOST|TOOLWINDOW|NOACTIVATE 保证置顶且不抢焦点；WS_POPUP 无边框
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
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

        // D2D/DComp 渲染资源：硬件失败 → WARP 兜底 → 再失败 → degraded（永不显示）。
        match create_render_state(hwnd) {
            Some(state) => self.render = Some(state),
            None => {
                self.degraded = true;
                crate::log::log_line("[candwin] D2D/DComp 初始化失败：候选窗降级隐藏（不显示）");
            }
        }
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

    /// 按当前 snapshot 重算尺寸 → 定位（caret 或原位）→ 无激活移动 → 同步上屏。
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
        // SAFETY: 仅移动/改尺寸，不激活（SWP_NOACTIVATE），保持 z 序（置顶组内不变）
        let _ = unsafe { SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER) };
        self.upload(&surf);
    }

    /// Surface → DComp surface（首次/尺寸变化时创建 + 重挂 SetContent）→ 上屏。
    /// 失败记日志，静默降级（不 panic）。
    fn upload(&mut self, surf: &iuv_ui::Surface) {
        let Some(state) = self.render.as_mut() else {
            return;
        };
        if state.surface.is_none() || state.surface_w != surf.w || state.surface_h != surf.h {
            // SAFETY: 创建/重建合成 surface：DWM 合成 per-pixel 透明
            // （DXGI_ALPHA_MODE_PREMULTIPLIED；格式 B8G8R8A8_UNORM 即 premultiplied 载体，
            // windows-rs 无 *_PREMULTIPLIED 常量——DXGI 语义由 alpha mode 表达）。
            match unsafe {
                state.device.CreateSurface(
                    surf.w,
                    surf.h,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                )
            } {
                Ok(surface) => {
                    // SAFETY: 视觉树内容 = surface（DWM 直读像素缓冲）
                    if unsafe { state.visual.SetContent(&surface) }.is_err() {
                        crate::log::log_line("[candwin] SetContent 失败");
                        return;
                    }
                    state.surface = Some(surface);
                    state.surface_w = surf.w;
                    state.surface_h = surf.h;
                }
                Err(e) => {
                    crate::log::log_line(&format!("[candwin] CreateSurface 失败：{e:?}"));
                    return;
                }
            }
        }
        let Some(surface) = state.surface.as_ref() else {
            return;
        };
        // SAFETY: BeginDraw 返回 ID2D1DeviceContext（iid 由 windows-rs 泛型填充）；
        // ctx 在 EndDraw 前释放（作用域块），updateoffset 输出本次更新的原点。
        let mut offset = POINT::default();
        let ctx: ID2D1DeviceContext = match unsafe { surface.BeginDraw(None, &mut offset) } {
            Ok(ctx) => ctx,
            Err(e) => {
                crate::log::log_line(&format!("[candwin] BeginDraw 失败：{e:?}"));
                return;
            }
        };
        {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                ..Default::default()
            };
            let size = D2D_SIZE_U {
                width: surf.w,
                height: surf.h,
            };
            // SAFETY: iuv-ui Surface 契约 = premultiplied BGRA、无 stride 填充 → 直供 1:1
            let bitmap = match unsafe {
                ctx.CreateBitmap(
                    size,
                    Some(surf.pixels.as_ptr() as *const core::ffi::c_void),
                    surf.w * 4,
                    &props,
                )
            } {
                Ok(b) => b,
                Err(e) => {
                    crate::log::log_line(&format!("[candwin] CreateBitmap 失败：{e:?}"));
                    let _ = unsafe { surface.EndDraw() };
                    return;
                }
            };
            let dest = D2D_RECT_F {
                left: offset.x as f32,
                top: offset.y as f32,
                right: offset.x as f32 + surf.w as f32,
                bottom: offset.y as f32 + surf.h as f32,
            };
            // SAFETY: 1:1 像素映射（NEAREST_NEIGHBOR 禁插值，与 iuv-ui 像素精确对应）
            let _ = unsafe {
                ctx.DrawBitmap(
                    &bitmap,
                    Some(&dest),
                    1.0,
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                    None,
                )
            };
        }
        // ctx 已释放 → EndDraw → Commit（DWM 合成一帧）
        if unsafe { surface.EndDraw() }.is_err() {
            crate::log::log_line("[candwin] EndDraw 失败");
            return;
        }
        if unsafe { state.device.Commit() }.is_err() {
            crate::log::log_line("[candwin] Commit 失败");
        }
    }

    /// WM_PAINT 统一入口：重渲染当前 snapshot → 防御性尺寸对齐 → 上屏。
    /// BeginPaint/EndPaint 由调用方（wnd_proc）负责校验区管理。
    fn paint(&mut self) {
        let Some(surf) = self.frame() else {
            return;
        };
        // 防御：尺寸与窗口矩形不一致（理论不发生，WM_PAINT 迟到）→ 先改窗尺寸再上屏
        // SAFETY: GetWindowRect 读当前窗口矩形
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_ok() {
            let w = (rc.right - rc.left) as u32;
            let h = (rc.bottom - rc.top) as u32;
            if w != surf.w || h != surf.h {
                // SAFETY: 仅改尺寸，不激活（SWP_NOACTIVATE），保持 z 序
                let _ = unsafe {
                    SetWindowPos(
                        self.hwnd,
                        None,
                        rc.left,
                        rc.top,
                        surf.w as i32,
                        surf.h as i32,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    )
                };
            }
        }
        self.upload(&surf);
    }
}

/// 创建 D3D11（硬件 → WARP 兜底）+ D2D1.1 设备 + DComp 设备/目标/视觉。
/// 失败返回 None（调用方置 degraded，候选窗永不显示）。
fn create_render_state(hwnd: HWND) -> Option<RenderState> {
    // SAFETY: D3D11CreateDevice 标准创建设备调用；NULL adapter = 首选硬件。
    // BGRA_SUPPORT 是 D2D 互操作的硬性要求（D2D1CreateDevice 前必须置位）。
    let mut d3d: Option<ID3D11Device> = None;
    let mut last = None;
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        last = Some(driver);
        let r = unsafe {
            D3D11CreateDevice(
                Option::<&IDXGIAdapter>::None,
                driver,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[
                    D3D_FEATURE_LEVEL_11_1,
                    D3D_FEATURE_LEVEL_11_0,
                    D3D_FEATURE_LEVEL_10_0,
                ]),
                D3D11_SDK_VERSION,
                Some(&mut d3d as *mut Option<ID3D11Device>),
                None,
                None,
            )
        };
        if r.is_ok() {
            break;
        }
    }
    let d3d = match d3d {
        Some(d) => d,
        None => {
            crate::log::log_line(&format!(
                "[candwin] D3D11CreateDevice 失败（{last:?} 与 WARP 均不可用）"
            ));
            return None;
        }
    };
    // SAFETY: ID3D11Device → IDXGIDevice（D2D/DComp 都吃 DXGI 设备）
    let dxgi: IDXGIDevice = match d3d.cast() {
        Ok(d) => d,
        Err(e) => {
            crate::log::log_line(&format!("[candwin] 获取 IDXGIDevice 失败：{e:?}"));
            return None;
        }
    };
    // SAFETY: D2D1CreateDevice 标准调用（D2D1.1 路径，无需 D2D1CreateFactory）
    let d2d: ID2D1Device = match unsafe { D2D1CreateDevice(&dxgi, None) } {
        Ok(d) => d,
        Err(e) => {
            crate::log::log_line(&format!("[candwin] D2D1CreateDevice 失败：{e:?}"));
            return None;
        }
    };
    // SAFETY: DCompositionCreateDevice 标准调用；返回类型泛型指定 IDCompositionDevice
    let device: IDCompositionDevice =
        match unsafe { DCompositionCreateDevice::<_, IDCompositionDevice>(&dxgi) } {
            Ok(d) => d,
            Err(e) => {
                crate::log::log_line(&format!("[candwin] DCompositionCreateDevice 失败：{e:?}"));
                return None;
            }
        };
    // SAFETY: topmost=true = 视觉树置顶合成（配合 WS_EX_TOPMOST 窗口）
    let target: IDCompositionTarget = match unsafe { device.CreateTargetForHwnd(hwnd, true) } {
        Ok(t) => t,
        Err(e) => {
            crate::log::log_line(&format!("[candwin] CreateTargetForHwnd 失败：{e:?}"));
            return None;
        }
    };
    // SAFETY: 视觉树根 = visual；surface 在首次上屏时创建并 SetContent
    let visual: IDCompositionVisual = match unsafe { device.CreateVisual() } {
        Ok(v) => v,
        Err(e) => {
            crate::log::log_line(&format!("[candwin] CreateVisual 失败：{e:?}"));
            return None;
        }
    };
    if unsafe { target.SetRoot(&visual) }.is_err() {
        crate::log::log_line("[candwin] SetRoot 失败");
        return None;
    }
    Some(RenderState {
        // surface 首次上屏时按真实尺寸创建（upload 尺寸分支）
        surface: None,
        visual,
        target,
        device,
        d2d,
        d3d,
        surface_w: 0,
        surface_h: 0,
    })
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
        if self.degraded {
            return; // 渲染资源创建失败：静默不显示（输入法主体不受影响）
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
        if self.hwnd.is_invalid() || !self.visible || self.degraded {
            return;
        }
        self.apply_layout_and_pos(None);
    }

    fn move_to(&mut self, caret: CaretRect) {
        if self.hwnd.is_invalid() || !self.visible || self.degraded {
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
            // SAFETY: 仅移动（SWP_NOSIZE），不激活
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
        // render / text 字段按声明序自然 drop（COM 引用计数释放，
        // 顺序 surface → visual → target → device → d2d → d3d）。
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

/// 类窗口过程：WM_PAINT 走 iuv-ui 渲染 + DComp 上屏（BeginPaint 校验区管理）；
/// WM_ERASEBKGND 返回 1（DComp 合成窗口无 GDI 背景可擦）；
/// WM_NCHITTEST 按圆角几何判定（圆角外 HTTRANSPARENT 点击穿透）；
/// 鼠标交互：WM_MOUSEACTIVATE 显式 MA_NOACTIVATE（点击候选窗绝不改变激活——
/// 无 owner popup 的激活转移会让宿主（WinUI3 记事本实测）失活崩 TSF，2026-08-13）；
/// WM_MOUSEMOVE 悬停命中 → 本地高亮 + 回调同步会话；WM_LBUTTONDOWN 命中 → 点击回调
/// 选词上屏；其余鼠标消息一律吞掉（不回 DefWindowProc，杜绝默认行为）。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: BeginPaint 仅允许在 WM_PAINT 内调用；hwnd 由消息循环保证有效。
            // DComp 呈现不画进窗口 DC，但必须成对调用以校验更新区（防 WM_PAINT 风暴）。
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !hdc.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            }
            if let Some(wnd) = get_self_mut(hwnd) {
                wnd.paint();
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
                        // SAFETY: 悬停行变化 → 本地重绘高亮（WM_PAINT 走全量重渲染）
                        let _ =
                            windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
                        let _ = windows::Win32::Graphics::Gdi::UpdateWindow(hwnd);
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
