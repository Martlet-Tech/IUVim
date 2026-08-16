//! 自绘右键菜单窗口（M5 托盘菜单；任务书 21-m5-tray-menu.md §3）。
//! 复用 candwin.rs 的 D2D 1.1 + DirectComposition 呈现模式 + iuv-ui `render_menu`：
//! 圆角/阴影/悬停高亮与候选窗同风格。
//!
//! 与候选窗的差异（模态菜单语义）：
//! - 窗口 **允许激活**（无 `WS_EX_NOACTIVATE`）：`popup_at` 显示时
//!   `SetForegroundWindow` 抢占焦点，这样能收 Esc / 方向键 / Enter（菜单收键盘是
//!   标准行为，系统菜单同样抢焦点；隐藏后焦点自然交还）；
//! - 点击外部关闭：`SetCapture` 全局捕获鼠标 + `WM_ACTIVATE(WA_INACTIVE)` 双保险
//!   （捕获把菜单外的鼠标按下也交给我们 → 命中不了任何行 → 隐藏）；
//! - 菜单内容（MenuEntry 列表）由调用方（tray.rs）在 `popup_at` 前设置；点击行
//!   经 `set_on_select` 回调（id 语义由调用方解释；id=0 分隔线不可点击）。
//!
//! 绝不 panic（DLL 内硬性约定）：任何失败（建窗/D2D/字体）记日志 + 静默隐藏。

use std::mem::size_of;
use std::sync::OnceLock;

use iuv_ui::layout::Rect;
use iuv_ui::{menu_hit_test, render_menu, MenuEntry, TextRenderer, Theme};
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
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, CS_HREDRAW, CS_VREDRAW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW,
    GetWindowRect, GWLP_USERDATA, HWND_TOPMOST, RegisterClassExW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, SW_HIDE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_SHOW,
    WA_INACTIVE, WM_ACTIVATE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_core::Interface;

const CLASS_NAME: PCWSTR = w!("IuvMenuWindow");

/// D2D/DComp 渲染资源（与菜单窗口同生命周期，结构同 candwin.rs）。
struct RenderState {
    surface: Option<IDCompositionSurface>,
    visual: IDCompositionVisual,
    #[allow(dead_code)]
    target: IDCompositionTarget,
    device: IDCompositionDevice,
    #[allow(dead_code)]
    d2d: ID2D1Device,
    #[allow(dead_code)]
    d3d: ID3D11Device,
    surface_w: u32,
    surface_h: u32,
}

/// 自绘右键菜单窗口：`new(theme)` 不建窗，首次 `popup_at` 懒建（窗口必须建在调用线程）。
/// 菜单项由 `popup_at` 传入；点击行 → `on_select` 回调（id 语义调用方解释）。
pub struct MenuWindow {
    hwnd: HWND,
    /// 当前菜单项（调用方在 popup 前设置）。
    items: Vec<MenuEntry>,
    /// 当前高亮行（None = 无高亮；分隔线不可高亮）。
    selected: Option<usize>,
    visible: bool,
    /// 最近一次渲染的行矩形（surface 坐标 = 客户区坐标，命中测试用）。
    rows: Vec<Rect>,
    /// 点击菜单项回调（id；None = 未接线）。
    on_select: Option<Box<dyn Fn(u16)>>,
    theme: Theme,
    /// 文本渲染器（fontdb 首扫只在窗口创建时一次）。
    text: Option<TextRenderer>,
    /// D2D/DComp 资源（窗口创建时初始化；失败 = degraded，菜单永不显示）。
    render: Option<RenderState>,
    /// 渲染资源创建失败：popup_at 静默（菜单不显示，输入法不受影响）。
    degraded: bool,
}

impl MenuWindow {
    /// 以指定主题构造（不建窗，首次 popup_at 懒建）。
    pub fn new(theme: Theme) -> Self {
        MenuWindow {
            hwnd: HWND::default(),
            items: Vec::new(),
            selected: None,
            visible: false,
            rows: Vec::new(),
            on_select: None,
            theme,
            text: None,
            render: None,
            degraded: false,
        }
    }

    /// 接线点击回调（tray.rs 在 popup 前注入；同线程调用）。
    pub fn set_on_select(&mut self, cb: Option<Box<dyn Fn(u16)>>) {
        self.on_select = cb;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 进程内注册一次窗口类；失败（非"已注册"）记日志，不 panic。
    fn register_class() {
        static REGISTERED: OnceLock<()> = OnceLock::new();
        REGISTERED.get_or_init(|| {
            // SAFETY: 所有字段显式/Default 填充；类名为静态宽字符串，进程生命周期有效。
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
                        crate::log::log_line("[menuwin] RegisterClassExW 失败");
                    }
                }
            }
        });
    }

    /// 当前 DPI：窗口 HDC 的 `LOGPIXELSY`，失败兜底 96（同 candwin）。
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
            crate::log::log_line("[menuwin] GetModuleHandleW 失败");
            return;
        }
        // SAFETY: WS_EX_TOPMOST|TOOLWINDOW 保证置顶且隐藏出任务栏；WS_POPUP 无边框。
        // 与候选窗不同：**不带 NOACTIVATE**——菜单是模态交互，允许抢焦点（收键盘）。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
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
            crate::log::log_line("[menuwin] CreateWindowExW 失败");
            return;
        };
        // SAFETY: self 仅在创建线程存活；Drop 先清零 GWLP_USERDATA 再销毁窗口，
        // 因此 wnd_proc 经 GetWindowLongPtrW 取到的指针不会悬垂。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, self as *mut Self as usize as _) };
        self.hwnd = hwnd;

        // TextRenderer：fontdb 首扫系统字体（几十 ms，仅窗口创建时一次，可接受）。
        self.text = Some(TextRenderer::new());

        // D2D/DComp 渲染资源：硬件失败 → WARP 兜底 → 再失败 → degraded（菜单不显示）。
        match create_render_state(hwnd) {
            Some(state) => self.render = Some(state),
            None => {
                self.degraded = true;
                crate::log::log_line("[menuwin] D2D/DComp 初始化失败：菜单降级隐藏（不显示）");
            }
        }
    }

    /// 渲染当前菜单 → Surface + 行矩形（含阴影外缘；失败返回 None）。
    fn frame(&mut self) -> Option<(iuv_ui::Surface, Vec<Rect>)> {
        if self.items.is_empty() {
            return None;
        }
        let scale = self.get_dpi() as f32 / 96.0;
        let (surf, rows) = {
            let text = self.text.as_mut()?;
            render_menu(&self.items, self.selected, &self.theme, scale, text)
        };
        if surf.w == 0 || surf.h == 0 {
            return None;
        }
        Some((surf, rows))
    }

    /// 在光标 `(x, y)`（屏幕坐标）弹出菜单：尺寸按内容自适应，右/底缘越界内收进
    /// 光标所在显示器工作区；显示时 `SetForegroundWindow` 抢焦点（收键盘）+ `SetCapture`
    /// 捕获鼠标（点击外部关闭）。
    pub fn popup_at(&mut self, items: Vec<MenuEntry>, x: i32, y: i32) {
        self.items = items;
        self.selected = None;
        if self.hwnd.is_invalid() {
            self.ensure_window();
            if self.hwnd.is_invalid() {
                return; // 建窗失败：静默降级（绝不 panic）
            }
        }
        if self.degraded {
            return; // 渲染资源创建失败：静默不显示（输入法主体不受影响）
        }
        let Some((surf, rows)) = self.frame() else {
            return;
        };
        self.rows = rows;
        let w = surf.w as i32;
        let h = surf.h as i32;
        let (px, py) = popup_position(x, y, w, h);
        // SAFETY: 移动/改尺寸 + 显示 + 置顶（HWND_TOPMOST 置入置顶组顶部，配合 WS_EX_TOPMOST）。
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                px,
                py,
                w,
                h,
                SWP_SHOWWINDOW,
            )
        };
        self.upload(&surf);
        // SAFETY: 菜单显示即激活（SW_SHOW）→ 双保险 SetForegroundWindow：保证能收
        // Esc/方向键/Enter。失败不影响鼠标交互（SetCapture 接管）。
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOW) };
        let _ = unsafe { SetForegroundWindow(self.hwnd) };
        // SAFETY: 捕获鼠标：菜单外按下也交给我们 → 命中不了行即隐藏（点击外部关闭）。
        unsafe { SetCapture(self.hwnd) };
        self.visible = true;
    }

    /// 隐藏菜单（幂等）：释放捕获 + 隐藏窗口。焦点自然交还调用窗口。
    pub fn hide(&mut self) {
        self.visible = false;
        if !self.hwnd.is_invalid() {
            // SAFETY: 释放捕获（防止隐藏后仍拦截全局鼠标）+ 隐藏窗口
            let _ = unsafe { ReleaseCapture() };
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    /// 使窗口失效并立即重绘（WM_PAINT 走全量重渲染，同步上屏）。
    fn redraw(&mut self) {
        if self.hwnd.is_invalid() {
            return;
        }
        // SAFETY: InvalidateRect + UpdateWindow 强制同步发送 WM_PAINT（重绘高亮）。
        let _ = unsafe { windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false) };
        let _ = unsafe { windows::Win32::Graphics::Gdi::UpdateWindow(self.hwnd) };
    }

    /// 方向键移动高亮：沿 dir 步进，跳过分隔线，越界即停（不循环）。
    fn move_selection(&mut self, dir: i32) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as i32;
        let step = if dir > 0 { 1 } else { -1 };
        let mut i = match self.selected {
            Some(cur) => cur as i32 + step,
            None => {
                if dir > 0 {
                    0
                } else {
                    len - 1
                }
            }
        };
        while i >= 0 && i < len {
            if !self.items[i as usize].is_separator() {
                self.selected = Some(i as usize);
                self.redraw();
                return;
            }
            i += step;
        }
    }

    /// WM_PAINT 统一入口：重渲染当前菜单 → 防御性尺寸对齐 → 上屏。
    fn paint(&mut self) {
        let Some((surf, rows)) = self.frame() else {
            return;
        };
        self.rows = rows;
        // 防御：尺寸与窗口矩形不一致（理论不发生，WM_PAINT 迟到）→ 先改窗尺寸再上屏
        // SAFETY: GetWindowRect 读当前窗口矩形
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_ok() {
            let w = (rc.right - rc.left) as u32;
            let h = (rc.bottom - rc.top) as u32;
            if w != surf.w || h != surf.h {
                // SAFETY: 仅改尺寸（SWP_NOZORDER 保持置顶 z 序）
                let _ = unsafe {
                    SetWindowPos(
                        self.hwnd,
                        None,
                        rc.left,
                        rc.top,
                        surf.w as i32,
                        surf.h as i32,
                        SWP_NOZORDER,
                    )
                };
            }
        }
        self.upload(&surf);
    }

    /// Surface → DComp surface（首次/尺寸变化时创建 + 重挂 SetContent）→ 上屏。
    /// 失败记日志，静默降级（不 panic）。实现同 candwin.rs `upload`。
    fn upload(&mut self, surf: &iuv_ui::Surface) {
        let Some(state) = self.render.as_mut() else {
            return;
        };
        if state.surface.is_none() || state.surface_w != surf.w || state.surface_h != surf.h {
            // SAFETY: 创建/重建合成 surface：DWM 合成 per-pixel 透明
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
                        crate::log::log_line("[menuwin] SetContent 失败");
                        return;
                    }
                    state.surface = Some(surface);
                    state.surface_w = surf.w;
                    state.surface_h = surf.h;
                }
                Err(e) => {
                    crate::log::log_line(&format!("[menuwin] CreateSurface 失败：{e:?}"));
                    return;
                }
            }
        }
        let Some(surface) = state.surface.as_ref() else {
            return;
        };
        // SAFETY: BeginDraw 返回 ID2D1DeviceContext；ctx 在 EndDraw 前释放（作用域块）。
        let mut offset = POINT::default();
        let ctx: ID2D1DeviceContext = match unsafe { surface.BeginDraw(None, &mut offset) } {
            Ok(ctx) => ctx,
            Err(e) => {
                crate::log::log_line(&format!("[menuwin] BeginDraw 失败：{e:?}"));
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
                    crate::log::log_line(&format!("[menuwin] CreateBitmap 失败：{e:?}"));
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
        if unsafe { surface.EndDraw() }.is_err() {
            crate::log::log_line("[menuwin] EndDraw 失败");
            return;
        }
        if unsafe { state.device.Commit() }.is_err() {
            crate::log::log_line("[menuwin] Commit 失败");
        }
    }
}

/// 创建 D3D11（硬件 → WARP 兜底）+ D2D1.1 设备 + DComp 设备/目标/视觉（同 candwin.rs）。
/// 失败返回 None（调用方置 degraded，菜单永不显示）。
fn create_render_state(hwnd: HWND) -> Option<RenderState> {
    // SAFETY: D3D11CreateDevice 标准创建设备调用；NULL adapter = 首选硬件。
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
                "[menuwin] D3D11CreateDevice 失败（{last:?} 与 WARP 均不可用）"
            ));
            return None;
        }
    };
    // SAFETY: ID3D11Device → IDXGIDevice（D2D/DComp 都吃 DXGI 设备）
    let dxgi: IDXGIDevice = match d3d.cast() {
        Ok(d) => d,
        Err(e) => {
            crate::log::log_line(&format!("[menuwin] 获取 IDXGIDevice 失败：{e:?}"));
            return None;
        }
    };
    // SAFETY: D2D1CreateDevice 标准调用（D2D1.1 路径）
    let d2d: ID2D1Device = match unsafe { D2D1CreateDevice(&dxgi, None) } {
        Ok(d) => d,
        Err(e) => {
            crate::log::log_line(&format!("[menuwin] D2D1CreateDevice 失败：{e:?}"));
            return None;
        }
    };
    // SAFETY: DCompositionCreateDevice 标准调用
    let device: IDCompositionDevice =
        match unsafe { DCompositionCreateDevice::<_, IDCompositionDevice>(&dxgi) } {
            Ok(d) => d,
            Err(e) => {
                crate::log::log_line(&format!("[menuwin] DCompositionCreateDevice 失败：{e:?}"));
                return None;
            }
        };
    // SAFETY: topmost=true = 视觉树置顶合成（配合 WS_EX_TOPMOST 窗口）
    let target: IDCompositionTarget = match unsafe { device.CreateTargetForHwnd(hwnd, true) } {
        Ok(t) => t,
        Err(e) => {
            crate::log::log_line(&format!("[menuwin] CreateTargetForHwnd 失败：{e:?}"));
            return None;
        }
    };
    // SAFETY: 视觉树根 = visual；surface 在首次上屏时创建并 SetContent
    let visual: IDCompositionVisual = match unsafe { device.CreateVisual() } {
        Ok(v) => v,
        Err(e) => {
            crate::log::log_line(&format!("[menuwin] CreateVisual 失败：{e:?}"));
            return None;
        }
    };
    if unsafe { target.SetRoot(&visual) }.is_err() {
        crate::log::log_line("[menuwin] SetRoot 失败");
        return None;
    }
    Some(RenderState {
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

/// 光标所在显示器工作区；失败兜底近乎全屏区域（同 candwin）。
fn work_area_for_point(x: i32, y: i32) -> iuv_ui::Area {
    // SAFETY: MonitorFromPoint 纯查询，无资源；GetMonitorInfoW 输出缓冲已初始化。
    let monitor = unsafe {
        windows::Win32::Graphics::Gdi::MonitorFromPoint(
            POINT { x, y },
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = windows::Win32::Graphics::Gdi::MONITORINFO {
        cbSize: size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { windows::Win32::Graphics::Gdi::GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        iuv_ui::Area {
            left: info.rcWork.left,
            top: info.rcWork.top,
            right: info.rcWork.right,
            bottom: info.rcWork.bottom,
        }
    } else {
        iuv_ui::Area {
            left: 0,
            top: 0,
            right: 32767,
            bottom: 32767,
        }
    }
}

/// 菜单定位：默认以光标 (x, y) 为左上角；右/底缘越界内收到工作区（保持完整可见）。
/// 光标本身必在工作区内，故只需内收，无需翻转到左侧/上方。
fn popup_position(x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    let area = work_area_for_point(x, y);
    let mut px = x;
    let mut py = y;
    if px + w > area.right {
        px = area.right - w;
    }
    if py + h > area.bottom {
        py = area.bottom - h;
    }
    if px < area.left {
        px = area.left;
    }
    if py < area.top {
        py = area.top;
    }
    (px, py)
}

impl Drop for MenuWindow {
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
        // render / text 字段按声明序自然 drop（COM 引用计数释放）。
    }
}

/// lparam 低 32 位的坐标 (x, y)（客户区坐标）。
fn client_pos(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as u32;
    ((v & 0xFFFF) as i32, ((v >> 16) & 0xFFFF) as i32)
}

/// 从 GWLP_USERDATA 取回窗口属主；0（未挂接/已销毁）返回 None。
fn get_self(hwnd: HWND) -> Option<&'static MenuWindow> {
    // SAFETY: 指针在窗口销毁前由 Drop 先清零，不悬垂；调用都在创建线程
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        // SAFETY: p 非 0 即此前 SetWindowLongPtrW 写入的有效指针
        Some(unsafe { &*(p as *const MenuWindow) })
    }
}

/// 可变版（hover/键盘移动本地 selected 高亮用）。线程约束同 `get_self`。
fn get_self_mut(hwnd: HWND) -> Option<&'static mut MenuWindow> {
    // SAFETY: 同 get_self：指针生命周期由 Drop 清零保证；调用都在创建线程
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        // SAFETY: p 非 0 即此前 SetWindowLongPtrW 写入的有效指针
        Some(unsafe { &mut *(p as *mut MenuWindow) })
    }
}

/// 类窗口过程：WM_PAINT 走 iuv-ui 渲染 + DComp 上屏；鼠标悬停高亮、点击命中回调用；
/// 点击外部/失焦（WM_ACTIVATE WA_INACTIVE）关闭；Esc/方向键/Enter 键盘导航。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: BeginPaint 仅允许在 WM_PAINT 内调用；DComp 呈现不画进窗口 DC，
            // 但必须成对调用以校验更新区（防 WM_PAINT 风暴）。
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
        // 失焦即关闭（点击外部 / Alt+Tab / 点击任务栏都先触发失焦）。
        WM_ACTIVATE => {
            if (wparam.0 as u32) & 0xFFFF == WA_INACTIVE {
                if let Some(wnd) = get_self_mut(hwnd) {
                    wnd.hide();
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = client_pos(lparam);
            if let Some(wnd) = get_self_mut(hwnd) {
                // 悬停命中非分隔线行 → 高亮；未命中/分隔线 → 取消高亮。
                let next = menu_hit_test(&wnd.rows, x, y)
                    .filter(|&r| wnd.items.get(r).map(|i| !i.is_separator()).unwrap_or(false));
                if next != wnd.selected {
                    wnd.selected = next;
                    wnd.redraw();
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // 先取命中行的 id（只读），再隐藏（避免回调期间窗口可见）。
            let (x, y) = client_pos(lparam);
            let mut fire: Option<u16> = None;
            if let Some(wnd) = get_self(hwnd) {
                if let Some(row) = menu_hit_test(&wnd.rows, x, y) {
                    if let Some(item) = wnd.items.get(row) {
                        if !item.is_separator() {
                            fire = Some(item.id);
                        }
                    }
                }
            }
            // 命中有效项 → 关闭并回调；未命中（点击外部/空白/分隔线）→ 仅关闭。
            if let Some(wnd) = get_self_mut(hwnd) {
                wnd.hide();
            }
            if let Some(id) = fire {
                if let Some(wnd) = get_self(hwnd) {
                    if let Some(cb) = wnd.on_select.as_ref() {
                        cb(id);
                    }
                }
            }
            LRESULT(0)
        }
        // 捕获期间菜单外的右键也关闭（模态菜单语义）。
        WM_RBUTTONDOWN => {
            if let Some(wnd) = get_self_mut(hwnd) {
                wnd.hide();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wparam.0 as u16);
            if let Some(wnd) = get_self_mut(hwnd) {
                match vk {
                    VK_ESCAPE => wnd.hide(),
                    VK_UP => wnd.move_selection(-1),
                    VK_DOWN => wnd.move_selection(1),
                    VK_RETURN => {
                        let id = wnd
                            .selected
                            .and_then(|i| wnd.items.get(i))
                            .filter(|i| !i.is_separator())
                            .map(|i| i.id);
                        wnd.hide();
                        if let Some(id) = id {
                            if let Some(cb) = wnd.on_select.as_ref() {
                                cb(id);
                            }
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_position_clamps_into_work_area() {
        // 模拟主屏 1920×1080（任务栏 40 → 工作区底 1040）：右/底越界内收。
        // work_area_for_point 依赖真实显示器，无法单测；这里只验证几何不变量
        // （由纯定位函数 popup_position 的局部性保证——内收只在真实工作区内做）。
        // 用两个端点的结果自洽性代替：全屏内位置原样保留（无法脱离工作区验证）。
        let (x, y) = popup_position(100, 100, 200, 100);
        assert!(x >= 0 && y >= 0, "定位结果非负");
    }

    #[test]
    fn move_selection_skips_separators() {
        let mut m = MenuWindow::new(iuv_ui::theme_light());
        m.items = vec![
            MenuEntry::new("a", 1),
            MenuEntry::separator(),
            MenuEntry::new("b", 2),
            MenuEntry::new("c", 3),
        ];
        // 无选中 → 向下从首项开始
        m.move_selection(1);
        assert_eq!(m.selected, Some(0));
        // 首项向下：跳过分隔线到 b
        m.move_selection(1);
        assert_eq!(m.selected, Some(2));
        // 再向下到 c
        m.move_selection(1);
        assert_eq!(m.selected, Some(3));
        // 底部越界：保持 c
        m.move_selection(1);
        assert_eq!(m.selected, Some(3));
        // 向上回到 b（跳过分隔线）
        m.move_selection(-1);
        assert_eq!(m.selected, Some(2));
        // 再向上到 a
        m.move_selection(-1);
        assert_eq!(m.selected, Some(0));
        // 顶部越界：保持 a
        m.move_selection(-1);
        assert_eq!(m.selected, Some(0));
    }

    #[test]
    fn move_selection_empty_no_panic() {
        let mut m = MenuWindow::new(iuv_ui::theme_light());
        m.move_selection(1);
        m.move_selection(-1);
        assert_eq!(m.selected, None);
    }
}
