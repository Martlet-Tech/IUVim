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

use iuv_ui::{menu_hit_test, render_menu, MenuEntry, TextRenderer, Theme};
use iuv_ui::layout::Rect;
use iuv_win::LayeredWindow;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW,
    WM_ACTIVATE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WA_INACTIVE,
};

const CLASS_NAME: PCWSTR = w!("IuvMenuWindow");

/// 自绘菜单窗口：置顶、圆角阴影、悬停高亮、点击回调、Esc/失焦/外部点击关闭。
/// `new` 不建窗；首次 `show_at` 懒建（窗口必须建在调用线程）。
pub struct MenuWindow {
    layered: iuv_win::LayeredWindow,
    /// 菜单项（构造时固定：设置/关于；未来候选窗右键菜单可复用本窗口传入其他项）。
    items: Vec<MenuEntry>,
    /// 悬停高亮行（None = 无高亮）。
    selected: Option<usize>,
    theme: Theme,
    text: Option<TextRenderer>,
    ulw: iuv_win::UlwSurface,
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
            layered: iuv_win::LayeredWindow::new(),
            items,
            selected: None,
            theme,
            text: None,
            ulw: iuv_win::UlwSurface::new(),
            rows: Vec::new(),
            on_select,
            visible: false,
        }
    }

    /// 在屏幕坐标 (x, y)（语言栏按钮位置）弹出菜单：懒建窗 → 渲染 → 定位内收 →
    /// ULW 上屏 → 显示并尝试激活（收键盘）。失败静默（记日志，不 panic）。
    pub fn show_at(&mut self, x: i32, y: i32) {
        if self.layered.hwnd.is_invalid() {
            self.ensure_window();
            if self.layered.hwnd.is_invalid() {
                return;
            }
        }
        let scale = self.layered.dpi() as f32 / 96.0;
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
        self.present(&surf, x, y, surf.w as i32, surf.h as i32);
        // SAFETY: SW_SHOW 显示；SetForegroundWindow 尝试激活（收 Esc/键盘）；
        // 被系统前台限制拒绝时菜单仍可见（鼠标交互可用，键盘导航降级）。
        let _ = unsafe { ShowWindow(self.layered.hwnd, SW_SHOW) };
        let _ = unsafe { SetForegroundWindow(self.layered.hwnd) };
        self.visible = true;
    }

    /// 隐藏（点击外部 / Esc / 失焦 / 点击菜单项后）。
    pub fn hide(&mut self) {
        self.visible = false;
        if !self.layered.hwnd.is_invalid() {
            // SAFETY: 隐藏菜单窗口
            let _ = unsafe { ShowWindow(self.layered.hwnd, SW_HIDE) };
        }
    }

    /// 原位重绘（悬停高亮变化）。
    fn repaint(&mut self) {
        if self.layered.hwnd.is_invalid() || !self.visible {
            return;
        }
        let scale = self.layered.dpi() as f32 / 96.0;
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
        if unsafe { GetWindowRect(self.layered.hwnd, &mut rc) }.is_err() {
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

    /// 懒建窗口 + 文本渲染器。失败仅记日志并保持窗口无效（后续调用静默降级）。
    fn ensure_window(&mut self) {
        if self.layered.is_created() {
            return;
        }
        // SAFETY: WS_EX_LAYERED = per-pixel alpha 合成；TOPMOST|TOOLWINDOW 置顶工具窗；
        // **无 NOACTIVATE**——菜单是模态交互，允许激活以收键盘（Esc）。
        // 外层指针生命周期由 LayeredWindow::drop 清零保证。
        let outer = self as *mut Self;
        self.layered.create(
            CLASS_NAME,
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            wnd_proc,
            outer,
            "menuwin",
        );
        if self.layered.is_created() {
            self.text = Some(TextRenderer::new());
        }
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

    /// ULW 呈现（共享模块 ulw.rs）。失败静默（记日志，不 panic）。
    fn present(&mut self, surf: &iuv_ui::Surface, x: i32, y: i32, w: i32, h: i32) {
        self.ulw.upload(self.layered.hwnd, surf, x, y, w, h, "[menuwin]");
    }
}

// 窗口销毁（先清 GWLP_USERDATA 再 DestroyWindow）由 LayeredWindow::drop 负责；
// ulw（DIB/DC）与 text 按字段声明序自然 drop。

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
            let (x, y) = LayeredWindow::client_pos(lparam);
            if let Some(wnd) = unsafe { LayeredWindow::get_self_mut::<MenuWindow>(hwnd) } {
                let hit = menu_hit_test(&wnd.rows, x, y);
                if hit != wnd.selected {
                    wnd.selected = hit;
                    wnd.repaint();
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = LayeredWindow::client_pos(lparam);
            if let Some(wnd) = unsafe { LayeredWindow::get_self_mut::<MenuWindow>(hwnd) } {
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
            if let Some(wnd) = unsafe { LayeredWindow::get_self_mut::<MenuWindow>(hwnd) } {
                wnd.hide();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 as u32 == VK_ESCAPE.0 as u32 {
                if let Some(wnd) = unsafe { LayeredWindow::get_self_mut::<MenuWindow>(hwnd) } {
                    wnd.hide();
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                if let Some(wnd) = unsafe { LayeredWindow::get_self_mut::<MenuWindow>(hwnd) } {
                    wnd.hide();
                }
            }
            LRESULT(0)
        }
        _ => LayeredWindow::default_wnd_proc(hwnd, msg, wparam, lparam),
    }
}
