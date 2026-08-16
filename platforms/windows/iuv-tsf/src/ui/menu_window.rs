//! 自绘右键菜单窗口（M5，2026-08-17 重建：ULW 呈现版）。
//!
//! 语言栏「中/英」按钮右键（`OnClick(TF_LBI_CLK_RIGHT)`，Win10/11 输入法区域不触发
//! `InitMenu`——2026-08-17 实测）→ 在点击坐标弹出本窗口。呈现复用候选窗已验证的
//! ULW 模式：iuv-ui `render_menu` 软件光栅 premultiplied BGRA → 32bpp DIB →
//! `UpdateLayeredWindow`（ULW_ALPHA）DWM per-pixel 合成，圆角/阴影与候选窗同风格。
//!
//! 菜单是**模态交互**（与候选窗"绝不抢焦点"不同）：允许激活（无 WS_EX_NOACTIVATE，
//! 显示时 SetForegroundWindow）以便收 Esc/键盘导航；点击外部/失焦自动隐藏。
//! 全部对外方法不返回错误：任何失败静默降级（隐藏/不显示），**绝不 panic**。

use std::mem::size_of;
use std::sync::OnceLock;

use iuv_ui::{menu_hit_test, render_menu, MenuEntry, TextRenderer, Theme};
use iuv_ui::layout::Rect;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, SIZE,
    WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
    MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    BI_RGB, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, HBITMAP, HDC, RGBQUAD,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowRect,
    RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, UpdateLayeredWindow,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, SW_HIDE, SW_SHOW, ULW_ALPHA,
    WM_ACTIVATE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WA_INACTIVE,
};

const CLASS_NAME: PCWSTR = w!("IuvMenuWindow");

/// ULW 呈现缓存（同 candwin：内存 DC + 32bpp 自顶向下 DIB，内存序 BGRA）。
struct UlwState {
    hdc_src: HDC,
    dib: HBITMAP,
    bits: *mut u8,
    w: u32,
    h: u32,
}

// SAFETY: UlwState 仅在创建线程使用，bits 指针不跨线程传递。
unsafe impl Send for UlwState {}

/// 自绘菜单窗口：置顶、圆角阴影、悬停高亮、点击回调、Esc/失焦/外部点击关闭。
/// `new` 不建窗；首次 `show_at` 懒建（窗口必须建在调用线程）。
pub struct MenuWindow {
    hwnd: HWND,
    /// 菜单项（构造时固定：设置/关于；未来候选窗右键菜单可复用本窗口传入其他项）。
    items: Vec<MenuEntry>,
    /// 悬停高亮行（None = 无高亮）。
    selected: Option<usize>,
    theme: Theme,
    text: Option<TextRenderer>,
    ulw: Option<UlwState>,
    /// 最近一次布局的行矩形（命中测试用）。
    rows: Vec<Rect>,
    /// 点击回调（菜单项 id；None = 未接线）。
    on_select: Option<Box<dyn Fn(u16)>>,
    visible: bool,
}

impl MenuWindow {
    pub fn new(
        theme: Theme,
        items: Vec<MenuEntry>,
        on_select: Option<Box<dyn Fn(u16)>>,
    ) -> Self {
        MenuWindow {
            hwnd: HWND::default(),
            items,
            selected: None,
            theme,
            text: None,
            ulw: None,
            rows: Vec::new(),
            on_select,
            visible: false,
        }
    }

    /// 在屏幕坐标 (x, y)（语言栏按钮位置）弹出菜单：懒建窗 → 渲染 → 定位内收 →
    /// ULW 上屏 → 显示并尝试激活（收键盘）。失败静默（记日志，不 panic）。
    pub fn show_at(&mut self, x: i32, y: i32) {
        if self.hwnd.is_invalid() {
            self.ensure_window();
            if self.hwnd.is_invalid() {
                return;
            }
        }
        let scale = self.get_dpi() as f32 / 96.0;
        let Some(text) = self.text.as_mut() else {
            return;
        };
        let (surf, rows) = render_menu(&self.items, self.selected, &self.theme, scale, text);
        if surf.w == 0 || surf.h == 0 {
            return;
        }
        self.rows = rows;
        // 定位：菜单左上角锚定点击点（右/下越界内收到工作区）。
        let (x, y) = self.clamp_pos(x, y, surf.w as i32, surf.h as i32);
        if !self.ensure_dib(&surf) {
            return;
        }
        self.present(&surf, x, y, surf.w as i32, surf.h as i32);
        // SAFETY: SW_SHOW 显示；SetForegroundWindow 尝试激活（收 Esc/键盘）；
        // 被系统前台限制拒绝时菜单仍可见（鼠标交互可用，键盘导航降级）。
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOW) };
        let _ = unsafe { SetForegroundWindow(self.hwnd) };
        self.visible = true;
    }

    /// 隐藏（点击外部 / Esc / 失焦 / 点击菜单项后）。
    pub fn hide(&mut self) {
        self.visible = false;
        if !self.hwnd.is_invalid() {
            // SAFETY: 隐藏菜单窗口
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 原位重绘（悬停高亮变化）。
    fn repaint(&mut self) {
        if self.hwnd.is_invalid() || !self.visible {
            return;
        }
        let scale = self.get_dpi() as f32 / 96.0;
        let Some(text) = self.text.as_mut() else {
            return;
        };
        let (surf, rows) = render_menu(&self.items, self.selected, &self.theme, scale, text);
        if surf.w == 0 || surf.h == 0 {
            return;
        }
        self.rows = rows;
        // SAFETY: GetWindowRect 读当前窗口矩形
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_err() {
            return;
        }
        if !self.ensure_dib(&surf) {
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

    /// 进程内注册一次窗口类。
    fn register_class() {
        static REGISTERED: OnceLock<()> = OnceLock::new();
        REGISTERED.get_or_init(|| {
            // SAFETY: 类名静态宽字符串，进程生命周期有效；失败仅记日志。
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

    /// 当前 DPI：窗口 HDC 的 LOGPIXELSY，失败兜底 96。
    fn get_dpi(&self) -> u32 {
        // SAFETY: hdc 由 GetDC 取得，使用后立即 ReleaseDC
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            if hdc.is_invalid() {
                return 96;
            }
            let dpi = windows::Win32::Graphics::Gdi::GetDeviceCaps(
                Some(hdc),
                windows::Win32::Graphics::Gdi::LOGPIXELSY,
            );
            ReleaseDC(Some(self.hwnd), hdc);
            if dpi <= 0 {
                96
            } else {
                dpi as u32
            }
        }
    }

    /// 懒建窗口 + 文本渲染器。失败仅记日志并保持 hwnd 无效。
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
        // SAFETY: WS_EX_LAYERED = per-pixel alpha 合成；TOPMOST|TOOLWINDOW 置顶工具窗；
        // **无 NOACTIVATE**——菜单是模态交互，允许激活以收键盘（Esc）。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
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
        // SAFETY: self 仅在创建线程存活；Drop 先清零 GWLP_USERDATA 再销毁窗口。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, self as *mut Self as usize as _) };
        self.hwnd = hwnd;
        self.text = Some(TextRenderer::new());
    }

    /// 定位：菜单左上角锚定 (x, y)；右/下越界内收到工作区（光标所在显示器优先）。
    fn clamp_pos(&self, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
        let mut area = RECT {
            left: 0,
            top: 0,
            right: 32767,
            bottom: 32767,
        };
        // SAFETY: MonitorFromPoint 纯查询；GetMonitorInfoW 输出缓冲已初始化。
        let monitor =
            unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            area = info.rcWork;
        }
        let x = if x + w > area.right { area.right - w } else { x };
        let y = if y + h > area.bottom { area.bottom - h } else { y };
        (x.max(area.left), y.max(area.top))
    }

    /// ULW 上屏（同 candwin：DIB 直拷 + UpdateLayeredWindow 一次定位/定尺寸/合成）。
    fn present(&mut self, surf: &iuv_ui::Surface, x: i32, y: i32, w: i32, h: i32) {
        let Some(ulw) = self.ulw.as_ref() else {
            return;
        };
        // SAFETY: bits 由 ensure_dib 以 surf 尺寸创建，拷贝长度 = surf 全量，不越界。
        unsafe {
            std::ptr::copy_nonoverlapping(
                surf.pixels.as_ptr(),
                ulw.bits,
                (surf.w as usize) * (surf.h as usize) * 4,
            );
        }
        // SAFETY: hdcDst = 桌面 DC；ULW_ALPHA + AC_SRC_ALPHA per-pixel premultiplied 合成。
        let dst = POINT { x, y };
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let hdc_dst = unsafe { GetDC(None) };
        if hdc_dst.is_invalid() {
            crate::log::log_line("[menuwin] GetDC(桌面) 失败");
            return;
        }
        let r = unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                Some(hdc_dst),
                Some(&dst),
                Some(&size),
                Some(ulw.hdc_src),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        };
        // SAFETY: GetDC/ReleaseDC 配对
        let _ = unsafe { ReleaseDC(None, hdc_dst) };
        if r.is_err() {
            crate::log::log_line(&format!(
                "[menuwin] UpdateLayeredWindow 失败：{:?}",
                r.unwrap_err()
            ));
        }
    }

    /// 确保 DIB 匹配 surf 尺寸（变化重建）。失败返回 false。
    fn ensure_dib(&mut self, surf: &iuv_ui::Surface) -> bool {
        let need = match self.ulw.as_ref() {
            Some(ulw) => ulw.w != surf.w || ulw.h != surf.h,
            None => true,
        };
        if !need {
            return true;
        }
        if let Some(ulw) = self.ulw.take() {
            // SAFETY: 先删 DC（选中对象不受影响），再删 DIB 对象。
            unsafe {
                let _ = DeleteDC(ulw.hdc_src);
                let _ = DeleteObject(ulw.dib.into());
            }
        }
        // SAFETY: CreateCompatibleDC(None) 以桌面 DC 为模板创建内存 DC。
        let hdc = unsafe { CreateCompatibleDC(None) };
        if hdc.is_invalid() {
            crate::log::log_line("[menuwin] CreateCompatibleDC 失败");
            return false;
        }
        // 32bpp 自顶向下（biHeight 负数）；内存序 = BGRA little-endian，与 iuv-ui 一致。
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: surf.w as i32,
                biHeight: -(surf.h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default(); 1],
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: bmi 与 bits 输出在调用期间有效；DIB_RGB_COLORS 平台调色板；无文件映射。
        let dib = match unsafe {
            CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
        } {
            Ok(d) => d,
            Err(e) => {
                crate::log::log_line(&format!("[menuwin] CreateDIBSection 失败：{e:?}"));
                // SAFETY: DC 未选中任何对象，可立即删除。
                unsafe {
                    let _ = DeleteDC(hdc);
                }
                return false;
            }
        };
        if bits.is_null() {
            // SAFETY: DIB 成功但无像素指针（理论不发生）：释放后降级。
            unsafe {
                let _ = DeleteObject(dib.into());
                let _ = DeleteDC(hdc);
            }
            return false;
        }
        // SAFETY: 选中 DIB 到内存 DC（ULW 的 hdcSrc 数据源）。
        unsafe {
            let _ = SelectObject(hdc, dib.into());
        }
        self.ulw = Some(UlwState {
            hdc_src: hdc,
            dib,
            bits: bits as *mut u8,
            w: surf.w,
            h: surf.h,
        });
        true
    }
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
        if let Some(ulw) = self.ulw.take() {
            // SAFETY: 同 ensure_dib 释放顺序（DC 已不引用 DIB）。
            unsafe {
                let _ = DeleteDC(ulw.hdc_src);
                let _ = DeleteObject(ulw.dib.into());
            }
        }
    }
}

/// 从 GWLP_USERDATA 取回窗口属主（可变版：悬停/点击/隐藏路径）。
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

/// lparam 低 32 位的客户区坐标 (x, y)。
fn client_pos(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as u32;
    ((v & 0xFFFF) as i32, ((v >> 16) & 0xFFFF) as i32)
}

/// 类窗口过程：ULW 内容不画窗口 DC（WM_PAINT 只校验）；悬停高亮重绘走 repaint；
/// 点击命中 → 回调 + 隐藏；Esc / 失焦 / 右键 → 隐藏；激活允许（模态菜单收键盘）。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: BeginPaint 仅允许在 WM_PAINT 内调用；成对调用校验更新区。
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !hdc.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = client_pos(lparam);
            if let Some(wnd) = get_self_mut(hwnd) {
                let hit = menu_hit_test(&wnd.rows, x, y);
                if hit != wnd.selected {
                    wnd.selected = hit;
                    wnd.repaint();
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = client_pos(lparam);
            if let Some(wnd) = get_self_mut(hwnd) {
                let action = menu_hit_test(&wnd.rows, x, y)
                    .and_then(|i| wnd.items.get(i))
                    .map(|e| e.id);
                wnd.hide();
                if let Some(id) = action {
                    if let Some(cb) = wnd.on_select.as_ref() {
                        cb(id);
                    }
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            if let Some(wnd) = get_self_mut(hwnd) {
                wnd.hide();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 as u32 == VK_ESCAPE.0 as u32 {
                if let Some(wnd) = get_self_mut(hwnd) {
                    wnd.hide();
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                if let Some(wnd) = get_self_mut(hwnd) {
                    wnd.hide();
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
