//! 悬停 tooltip 小窗（§6.6：独立 ULW 窗口，不抢焦点；无点击）。P2.6 自 toolbar.rs 拆出。

use iuv_ui::{render_tooltip, TextRenderer, Theme};
use iuv_win::UlwSurface;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSY};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, GetCursorPos, ShowWindow, SW_HIDE, SW_SHOWNA, HTTRANSPARENT,
    MA_NOACTIVATE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT,
};

use super::{clamp_to_work, CLASS_TIP};

/// 悬停 tooltip 小窗（§6.6：独立 ULW 窗口，不抢焦点；无点击）。
pub(super) struct TooltipWindow {
    hwnd: HWND,
    theme: Theme,
    text: Option<TextRenderer>,
    ulw: UlwSurface,
}

impl TooltipWindow {
    pub(super) fn new(theme: Theme) -> TooltipWindow {
        let hwnd = super::create_window(CLASS_TIP);
        let mut t = TooltipWindow {
            hwnd,
            theme,
            text: None,
            ulw: UlwSurface::new(),
        };
        if !hwnd.is_invalid() {
            t.text = Some(TextRenderer::new());
        }
        t
    }

    pub(super) fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.hide(); // 下轮悬停用新主题重绘（简单起见不缓存标签）
    }

    pub(super) fn show_near(&mut self, _theme: &Theme, label: &str, _bar: HWND) {
        if self.hwnd.is_invalid() {
            return;
        }
        let scale = self.scale();
        let Some(text) = self.text.as_mut() else {
            return;
        };
        let surf = render_tooltip(label, &self.theme, scale, text);
        if surf.w == 0 || surf.h == 0 {
            return;
        }
        // 定位：鼠标下方（错开工具栏）。
        // SAFETY: GetCursorPos 纯查询。
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return;
        }
        // 锚定光标处，右/下越界内收。
        let (x, y) = clamp_to_work(pt.x + 8, pt.y + 12, surf.w as i32, surf.h as i32);
        self.ulw
            .upload(self.hwnd, &surf, x, y, surf.w as i32, surf.h as i32, "[tooltip]");
        // SAFETY: SW_SHOWNA 显示但不激活。
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNA) };
    }

    pub(super) fn hide(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: 隐藏 tooltip 窗口。
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
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
}

impl Drop for TooltipWindow {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: 在创建线程销毁窗口。
            let _ = unsafe { DestroyWindow(self.hwnd) };
            self.hwnd = HWND::default();
        }
    }
}

/// tooltip 窗口过程：全屏点击穿透（HTTRANSPARENT，不接收任何鼠标）。
pub(super) unsafe extern "system" fn tip_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !hdc.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize), // tooltip 全程点击穿透
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}