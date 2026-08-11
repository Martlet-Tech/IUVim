//! GDI 候选窗实现。契约 01-contract.md §5、任务书 14-mod-ime-tsf-candwin.md。
//! 无边框 / 置顶 / **不抢焦点**（`WS_EX_NOACTIVATE` + `SW_SHOWNA`，绝不 `SetForegroundWindow`/`SetFocus`）；
//! `WM_PAINT` 内存 DC 双缓冲，一次 `BitBlt`，无闪烁；
//! 全部对外方法不返回错误：任何失败静默降级（隐藏窗口 / 不显示），**绝不 panic**。
//!
//! 已知限制（M1 接受，见任务书 §5 槽位）：
//! - DPI：冻结的 windows feature 集不含 `Win32_UI_HiDpi`，无 `GetDpiForWindow`，
//!   按任务书兜底走窗口 HDC `LOGPIXELSY`（进程 DPI 感知时即该监视器 DPI）。
//! - 页码指示与正文同字体（任务书"小字"留待 M4 主题化）。
//! - 窗口必须由调用线程创建/销毁（TSF 回调线程有消息循环，成立）；`Drop` 需在同一线程。

use std::mem::size_of;
use std::sync::OnceLock;

use super::{CandidateUi, CaretRect, UiSnapshot};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, SIZE,
    WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontIndirectW,
    CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, FrameRect, GetDC,
    GetDeviceCaps, GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect, MonitorFromPoint,
    MonitorFromWindow, ReleaseDC, SelectObject, SetBkColor, SetTextColor, TextOutW, UpdateWindow,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, HBRUSH, HDC, HFONT, HGDIOBJ,
    LOGFONTW, LOGPIXELSY, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    GetWindowRect, RegisterClassExW, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SystemParametersInfoW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, SPI_GETWORKAREA, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_ERASEBKGND, WM_PAINT,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

// ===== 主题常量（M4 主题槽位：集中于此，届时只动这里）=====
const BG_COLOR: COLORREF = COLORREF(0x00FF_FFFF); // 背景白
const TEXT_COLOR: COLORREF = COLORREF(0x001F_1F1F); // 正文近黑
const READING_COLOR: COLORREF = COLORREF(0x0066_6666); // reading 次要灰
const HL_BG: COLORREF = COLORREF(0x00D7_7800); // 高亮底 #0078D7
const HL_TEXT: COLORREF = COLORREF(0x00FF_FFFF); // 高亮字白
const PAGE_COLOR: COLORREF = COLORREF(0x0099_9999); // 页码灰
const BORDER_COLOR: COLORREF = COLORREF(0x00C0_C0C0); // 1px 外框浅灰（白底区分边界）

// ===== 布局常量 =====
const PAD_X: i32 = 8;
const PAD_Y: i32 = 4;
const ROW_GAP: i32 = 2;

// ===== 字体 =====
const FONT_FACE: &str = "Microsoft YaHei UI\0";
const FONT_PT: i32 = 14;

const CLASS_NAME: PCWSTR = w!("InputImeCandidateWindow");

/// 行矩形（窗口客户区坐标）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 纯布局计算：返回 `(窗口宽, 窗口高, 各行矩形)`。
/// 行序：`reading` → 各候选（`"N.候选"`）→ 页码（`page_count > 1` 时，右对齐）；
/// `reading` 为空时跳过该行。`measurer` 返回文本的 (宽, 高)。
pub fn layout(snap: &UiSnapshot, measurer: &dyn Fn(&str) -> (i32, i32)) -> (i32, i32, Vec<Rect>) {
    let mut rows: Vec<(String, i32, i32)> = Vec::new();
    if !snap.reading.is_empty() {
        let (w, h) = measurer(&snap.reading);
        rows.push((snap.reading.clone(), w, h));
    }
    for (i, cand) in snap.candidates.iter().enumerate() {
        let text = format!("{}.{}", i + 1, cand);
        let (w, h) = measurer(&text);
        rows.push((text, w, h));
    }
    let show_page = snap.page.page_count > 1;
    if show_page {
        let text = format!("{}/{}", snap.page.page + 1, snap.page.page_count);
        let (w, h) = measurer(&text);
        rows.push((text, w, h));
    }
    if rows.is_empty() {
        return (PAD_X * 2, PAD_Y * 2, Vec::new());
    }
    let content_w = rows.iter().map(|r| r.1).max().unwrap_or(0);
    let mut rects = Vec::with_capacity(rows.len());
    let mut y = PAD_Y;
    for (_, w, h) in &rows {
        rects.push(Rect {
            x: PAD_X,
            y,
            w: *w,
            h: *h,
        });
        y += h + ROW_GAP;
    }
    if show_page {
        if let Some(last) = rects.last_mut() {
            last.x = PAD_X + content_w - last.w;
        }
    }
    (content_w + PAD_X * 2, y - ROW_GAP + PAD_Y, rects)
}

/// GDI 自绘候选窗：无边框、置顶、不抢焦点。
/// `new()` 不建窗；首次 `show` 懒建（窗口必须建在调用线程）。
pub struct GdiCandidateWindow {
    hwnd: HWND,
    font: HFONT,
    snap: UiSnapshot,
    visible: bool,
    /// 最近一次定位用的光标锚点（update 超屏时翻屏用）。
    last_caret: Option<CaretRect>,
}

impl GdiCandidateWindow {
    pub fn new() -> Self {
        GdiCandidateWindow {
            hwnd: HWND::default(),
            font: HFONT::default(),
            snap: UiSnapshot::default(),
            visible: false,
            last_caret: None,
        }
    }

    /// 进程内注册一次窗口类；失败（非"已注册"）记日志，不 panic。
    fn register_class() {
        static REGISTERED: OnceLock<()> = OnceLock::new();
        REGISTERED.get_or_init(|| {
            // SAFETY: 所有字段显式/Default 填充；类名为静态宽字符串，进程生命周期有效。
            // 失败仅记日志（W0 桩 log.rs，Agent D 会落盘 %TEMP%\input-ime-tsf.log）。
            unsafe {
                let class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(wnd_proc),
                    hbrBackground: HBRUSH::default(),
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
            let hdc = GetDC(Some(self.hwnd));
            if hdc.is_invalid() {
                return 96;
            }
            let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSY);
            ReleaseDC(Some(self.hwnd), hdc);
            if dpi <= 0 {
                96
            } else {
                dpi as u32
            }
        }
    }

    /// 懒建窗口 + 字体；失败仅记日志并保持 `hwnd` 无效（后续调用静默降级）。
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
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, self as *mut Self as isize) };
        self.hwnd = hwnd;
        self.font = create_font(self.get_dpi());
    }

    /// 按当前 snapshot 重算尺寸 → 定位（caret 或原位）→ 无激活移动 → 同步重绘。
    fn apply_layout_and_pos(&mut self, caret: Option<CaretRect>) {
        // SAFETY: GetDC/ReleaseDC 配对使用
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            if hdc.is_invalid() {
                return;
            }
            let old_font = if !self.font.is_invalid() {
                SelectObject(hdc, self.font.into())
            } else {
                HGDIOBJ::default()
            };
            let (w, h, _) = layout(&self.snap, &|s| measure(hdc, s));
            if !old_font.is_invalid() {
                SelectObject(hdc, old_font);
            }
            ReleaseDC(Some(self.hwnd), hdc);

            let (x, y) = match caret {
                Some(c) => {
                    crate::log::log_line(&format!(
                        "[candwin] 定位输入：caret=({},{},{},{}) 窗口尺寸=({},{})",
                        c.x, c.y, c.w, c.h, w, h
                    ));
                    position_for(c, w, h)
                }
                None => {
                    let mut rc = RECT::default();
                    let _ = GetWindowRect(self.hwnd, &mut rc);
                    let (x, y) =
                        update_position((rc.left, rc.top), w, h, work_area_for(self.hwnd), self.last_caret);
                    crate::log::log_line(&format!(
                        "[candwin] 原位更新：GetWindowRect=({},{},{},{}) 窗口尺寸=({},{}) 修正后=({},{})",
                        rc.left, rc.top, rc.right, rc.bottom, w, h, x, y
                    ));
                    (x, y)
                }
            };
            crate::log::log_line(&format!(
                "[candwin] 定位结果：目标=({},{}) 尺寸=({},{})",
                x, y, w, h
            ));
            // SAFETY: 仅移动/改尺寸，不激活（SWP_NOACTIVATE），保持 z 序（置顶组内不变）
            let _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
            // SAFETY: 全量无效化并同步重绘（双缓冲，无闪烁）
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = UpdateWindow(self.hwnd);
        }
    }
}

impl Default for GdiCandidateWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl CandidateUi for GdiCandidateWindow {
    fn show(&mut self, snap: &UiSnapshot, caret: CaretRect) {
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
            crate::log::log_line(&format!(
                "[candwin] move_to：caret=({},{}) -> 目标=({},{}) 尺寸=({},{})",
                caret.x, caret.y, x, y, w, h
            ));
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
}

impl Drop for GdiCandidateWindow {
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
        if !self.font.is_invalid() {
            // SAFETY: DeleteObject 释放 GDI 字体对象
            let _ = unsafe { DeleteObject(self.font.into()) };
            self.font = HFONT::default();
        }
    }
}

/// 窗口所在显示器的物理工作区；失败兜底近乎全屏区域。
fn work_area_for(hwnd: HWND) -> RECT {
    // SAFETY: MonitorFromWindow 纯查询；GetMonitorInfoW 输出缓冲已初始化。
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 32767,
            bottom: 32767,
        }
    }
}

/// update 原位修正：候选内容变化导致窗口变高/变宽时，
/// - 当前位置 + 新高度超出工作区底 → 用最近一次 caret 重新定位（下方放不下自动翻到光标上方）
/// - 当前位置 + 新宽度超出工作区右缘 → 左移内收，保证完整可见
/// 无 caret 兜底贴工作区底；未超屏保持原位。
fn update_position(
    current: (i32, i32),
    w: i32,
    h: i32,
    work: RECT,
    last_caret: Option<CaretRect>,
) -> (i32, i32) {
    let (x, y) = current;
    if y + h <= work.bottom && x + w <= work.right {
        return (x, y);
    }
    match last_caret {
        Some(caret) => position_in_area(caret, w, h, work),
        None => {
            let x = if x + w > work.right { work.right - w } else { x };
            let y = if y + h > work.bottom { work.bottom - h } else { y };
            (x, y)
        }
    }
}

/// 按 caret 定位：默认放在 caret 下方；超出工作区（优先 caret 所在显示器，
/// SPI_GETWORKAREA 兜底）则右/下边界内收，下方放不下时翻到 caret 上方。
fn position_for(caret: CaretRect, w: i32, h: i32) -> (i32, i32) {
    let mut area = RECT::default();
    // 优先取光标所在显示器的物理工作区：GetTextExt 返回物理像素坐标，
    // SPI_GETWORKAREA 只返回主屏工作区（副屏打字时会把候选框 clamp 回主屏）。
    let monitor =
        // SAFETY: MonitorFromPoint 纯查询，无资源；caret 为本地值。
        unsafe { MonitorFromPoint(POINT { x: caret.x, y: caret.y }, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let got_work =
        // SAFETY: GetMonitorInfoW 输出缓冲 info 在调用前初始化且存活。
        unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool();
    if got_work {
        area = info.rcWork;
    } else {
        // 兜底：主屏工作区。
        // SAFETY: SPI_GETWORKAREA 需要可写 RECT；失败时兜底为近乎全屏的区域。
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut area as *mut RECT as *mut core::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        if ok.is_err() {
            area = RECT {
                left: 0,
                top: 0,
                right: 32767,
                bottom: 32767,
            };
        }
    }
    position_in_area(caret, w, h, area)
}

/// 纯函数定位：给定工作区 `area` 内计算窗口位置。
/// 默认 `caret` 下方（光标底 + CARET_GAP）；右/下边界内收；下方放不下翻到 `caret` 上方；
/// 上下都放不下时贴工作区边，保证窗口完整可见。
const CARET_GAP: i32 = 2;

fn position_in_area(caret: CaretRect, w: i32, h: i32, area: RECT) -> (i32, i32) {
    let mut x = caret.x;
    if x + w > area.right {
        x = area.right - w;
    }
    if x < area.left {
        x = area.left;
    }
    // caret.h=0 时（collapsed 光标）同样按 CARET_GAP 留间隙。
    let below = caret.y + caret.h + CARET_GAP;
    let mut y = if below + h <= area.bottom {
        below
    } else {
        caret.y - h // 下方放不下 → 翻到 caret 上方
    };
    if y < area.top {
        y = area.top;
    }
    if y > area.bottom - h {
        y = area.bottom - h; // 上下都不够 → 贴底，窗口完整可见
    }
    (x, y)
}

/// 按字体缩放的字号生成字体（pt → 像素：-((dpi*pt+36)/72)，等价 MulDiv 四舍五入）。
fn create_font(dpi: u32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -((dpi as i32 * FONT_PT + 36) / 72),
        lfWeight: 400,
        lfCharSet: DEFAULT_CHARSET,
        lfOutPrecision: OUT_DEFAULT_PRECIS,
        lfClipPrecision: CLIP_DEFAULT_PRECIS,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    let face: Vec<u16> = FONT_FACE.encode_utf16().collect();
    let n = face.len().min(lf.lfFaceName.len());
    lf.lfFaceName[..n].copy_from_slice(&face[..n]);
    // SAFETY: lf 在调用期间有效；CreateFontIndirectW 同步复制字体描述
    unsafe { CreateFontIndirectW(&lf) }
}

/// 测量单行文本 (宽, 高)；失败返回 (0, 0)。
fn measure(hdc: HDC, text: &str) -> (i32, i32) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    // SAFETY: hdc 有效；wide 与 size 在调用期间存活
    let ok = unsafe { GetTextExtentPoint32W(hdc, &wide, &mut size) };
    if ok.as_bool() {
        (size.cx, size.cy)
    } else {
        (0, 0)
    }
}

/// 逐行画文本；底色随行背景设置（Opaque 模式），高亮行先填底再白字。
fn draw_text(hdc: HDC, text: &str, x: i32, y: i32, color: COLORREF, bk: COLORREF) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: hdc 有效；wide 在调用期间存活
    unsafe {
        let _ = SetBkColor(hdc, bk);
        let _ = SetTextColor(hdc, color);
        let _ = TextOutW(hdc, x, y, &wide);
    }
}

/// 由布局计算当前 snapshot 的完整内容并绘制到 DC。
fn draw_content(hdc: HDC, snap: &UiSnapshot, w: i32, h: i32) {
    // SAFETY: hdc 有效；brush 用后即删
    unsafe {
        let bg = CreateSolidBrush(BG_COLOR);
        let rc = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        let _ = FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg.into());
        // 1px 外框（浅灰）：白底应用里区分候选窗边界；brush 用后即删。
        let border = CreateSolidBrush(BORDER_COLOR);
        let _ = FrameRect(hdc, &rc, border);
        let _ = DeleteObject(border.into());
    }
    let (_, _, rects) = layout(snap, &|s| measure(hdc, s));
    let mut i = 0usize;
    if !snap.reading.is_empty() {
        if let Some(r) = rects.get(i) {
            draw_text(hdc, &snap.reading, r.x, r.y, READING_COLOR, BG_COLOR);
            i += 1;
        }
    }
    for (ci, cand) in snap.candidates.iter().enumerate() {
        let Some(r) = rects.get(i) else {
            break; // 防御：布局行数与候选数不一致也不越界
        };
        let sel = ci == snap.selected;
        if sel {
            // SAFETY: 高亮行底色；brush 用后即删
            unsafe {
                let hl = CreateSolidBrush(HL_BG);
                let rr = RECT {
                    left: r.x,
                    top: r.y,
                    right: r.x + r.w,
                    bottom: r.y + r.h,
                };
                let _ = FillRect(hdc, &rr, hl);
                let _ = DeleteObject(hl.into());
            }
        }
        let text = format!("{}.{}", ci + 1, cand);
        let (fg, bk) = if sel {
            (HL_TEXT, HL_BG)
        } else {
            (TEXT_COLOR, BG_COLOR)
        };
        draw_text(hdc, &text, r.x, r.y, fg, bk);
        i += 1;
    }
    if snap.page.page_count > 1 {
        if let Some(r) = rects.get(i) {
            let text = format!("{}/{}", snap.page.page + 1, snap.page.page_count);
            draw_text(hdc, &text, r.x, r.y, PAGE_COLOR, BG_COLOR);
        }
    }
}

/// 从 GWLP_USERDATA 取回窗口属主；0（未挂接/已销毁）返回 None。
fn get_self(hwnd: HWND) -> Option<&'static GdiCandidateWindow> {
    // SAFETY: 指针在窗口销毁前由 Drop 先清零，不悬垂；调用都在创建线程
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        // SAFETY: p 非 0 即此前 SetWindowLongPtrW 写入的有效指针
        Some(unsafe { &*(p as *const GdiCandidateWindow) })
    }
}

/// 内存 DC 双缓冲绘制：整窗填充 → 逐行文本 → 一次 BitBlt。
fn paint(hwnd: HWND) {
    let Some(wnd) = get_self(hwnd) else {
        return;
    };
    // SAFETY: BeginPaint 仅允许在 WM_PAINT 内调用；hwnd 由消息循环保证有效
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            return;
        }
        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_err() {
            let _ = EndPaint(hwnd, &ps);
            return;
        }
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        if w <= 0 || h <= 0 {
            let _ = EndPaint(hwnd, &ps);
            return;
        }
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        if mem.is_invalid() || bmp.is_invalid() {
            if !bmp.is_invalid() {
                let _ = DeleteObject(bmp.into());
            }
            if !mem.is_invalid() {
                let _ = DeleteDC(mem);
            }
            let _ = EndPaint(hwnd, &ps);
            return;
        }
        let old_bmp = SelectObject(mem, bmp.into());
        if !wnd.font.is_invalid() {
            SelectObject(mem, wnd.font.into());
        }
        draw_content(mem, &wnd.snap, w, h);
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old_bmp);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// 类窗口过程：WM_PAINT 双缓冲自绘；WM_ERASEBKGND 直接返回 1（跳过擦背景，防闪烁）。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ime_core::PageInfo;

    fn fake_measurer(s: &str) -> (i32, i32) {
        (s.chars().count() as i32 * 10, 20)
    }

    fn snap(reading: &str, candidates: &[&str], page: usize, page_count: usize) -> UiSnapshot {
        UiSnapshot {
            reading: reading.to_string(),
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            selected: 0,
            page: PageInfo {
                page,
                page_count,
                page_size: 5,
                total: page_count * 5,
            },
        }
    }

    #[test]
    fn layout_single_page_rows_and_size() {
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 1);
        let (w, h, rects) = layout(&s, &fake_measurer);
        assert_eq!(rects.len(), 3, "reading + 2 候选");
        assert_eq!(w, 60 + PAD_X * 2, "最宽行 reading 6 字");
        assert_eq!(h, PAD_Y * 2 + 20 * 3 + ROW_GAP * 2);
        assert_eq!(
            rects[0],
            Rect {
                x: PAD_X,
                y: PAD_Y,
                w: 60,
                h: 20
            }
        );
        assert_eq!(rects[1].x, PAD_X);
        assert_eq!(rects[2].y, PAD_Y + (20 + ROW_GAP) * 2);
    }

    #[test]
    fn layout_multi_page_indicator_right_aligned() {
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 3);
        let (w, _, rects) = layout(&s, &fake_measurer);
        assert_eq!(rects.len(), 4, "reading + 2 候选 + 页码");
        let page_rect = *rects.last().unwrap();
        assert_eq!(
            page_rect.x,
            PAD_X + 60 - 30,
            "页码右对齐：x = PAD_X + content_w - 页码宽"
        );
        assert_eq!(page_rect.y, PAD_Y + (20 + ROW_GAP) * 3);
        assert_eq!(w, 60 + PAD_X * 2, "页码窄于最宽行，宽度不变");
    }

    #[test]
    fn layout_page_indicator_wider_than_rows() {
        let s = snap("ni", &["你"], 0, 100);
        let (w, _, rects) = layout(&s, &fake_measurer);
        let page_rect = *rects.last().unwrap();
        assert_eq!(w, 50 + PAD_X * 2, "页码 '1/100'=5 字 50px 最宽，撑开窗口");
        assert_eq!(page_rect.x, PAD_X, "页码自己最宽时从 PAD_X 起");
    }

    #[test]
    fn layout_empty_snapshot_no_rows() {
        let s = UiSnapshot::default();
        let (w, h, rects) = layout(&s, &fake_measurer);
        assert!(rects.is_empty());
        assert_eq!(w, PAD_X * 2);
        assert_eq!(h, PAD_Y * 2);
    }

    #[test]
    fn layout_skips_empty_reading() {
        let s = snap("", &["你好"], 0, 1);
        let (_, _, rects) = layout(&s, &fake_measurer);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].y, PAD_Y);
    }

    #[test]
    fn layout_candidate_wider_than_reading() {
        let s = snap("ni", &["你好", "泥嚎"], 0, 1);
        let (w, _, rects) = layout(&s, &fake_measurer);
        assert_eq!(w, 40 + PAD_X * 2, "候选行 '1.你好'=4 字 40px 最宽");
        assert_eq!(rects[1].x, PAD_X);
    }

    #[test]
    fn update_position_keeps_in_place_when_fits() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let c = CaretRect {
            x: 100,
            y: 700,
            w: 2,
            h: 20,
        };
        assert_eq!(
            update_position((100, 800), 200, 195, work, Some(c)),
            (100, 800),
            "当前位置 + 新高度不超屏 → 保持原位"
        );
    }

    #[test]
    fn update_position_flips_above_caret_when_overflow() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 900,
        };
        let c = CaretRect {
            x: 100,
            y: 800,
            w: 2,
            h: 20,
        };
        assert_eq!(
            update_position((100, 800), 200, 195, work, Some(c)),
            (100, 605),
            "窗口变高超屏 → 用 caret 重定位：下方放不下翻到光标上方"
        );
    }

    #[test]
    fn update_position_clamps_right_edge_when_wider() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 3138,
            bottom: 900,
        };
        let c = CaretRect {
            x: 3043,
            y: 757,
            w: 2,
            h: 20,
        };
        // 窗口变宽到 237：3043+237=3280 > 3138 → 左移内收，右缘对齐工作区。
        // y 按 caret 重定位：779 = 757+20+2（光标下方，不超底）。
        assert_eq!(
            update_position((3043, 562), 237, 60, work, Some(c)),
            (3138 - 237, 779),
            "变宽超右缘 → 左移内收，右缘对齐工作区"
        );
    }

    #[test]
    fn update_position_clamps_to_work_bottom_without_caret() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 900,
        };
        assert_eq!(
            update_position((100, 800), 200, 195, work, None),
            (100, 900 - 195),
            "无 caret 锚点兜底 → 贴工作区底，保证完整可见"
        );
    }

    #[test]
    fn position_below_caret_by_default() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let caret = CaretRect {
            x: 100,
            y: 100,
            w: 2,
            h: 20,
        };
        let (x, y) = position_in_area(caret, 200, 100, area);
        assert_eq!((x, y), (100, 122), "默认 caret 正下方（光标底 + 2px 间隙），不越界原样保留");
    }

    #[test]
    fn position_flips_above_caret_when_no_room_below() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        // caret 贴近屏幕底边：窗口应翻到 caret 上方
        let caret = CaretRect {
            x: 100,
            y: 1000,
            w: 2,
            h: 20,
        };
        let (x, y) = position_in_area(caret, 200, 100, area);
        assert_eq!(y, caret.y - 100);
        assert_eq!(x, caret.x, "x 未越界保持不变");
    }

    #[test]
    fn position_clamps_into_workarea() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        // caret 在右/下边缘：窗口右/下边界内收
        let caret = CaretRect {
            x: 1900,
            y: 1000,
            w: 2,
            h: 20,
        };
        let (x, y) = position_in_area(caret, 300, 200, area);
        assert_eq!(x, 1920 - 300, "右边界内收");
        assert_eq!(y, 1000 - 200, "下方放不下翻到上方");
        assert!(x + 300 <= area.right);
        assert!(y + 200 <= area.bottom);
    }

    #[test]
    fn position_clamps_to_area_edge_when_caret_fully_outside() {
        let area = RECT {
            left: 100,
            top: 100,
            right: 1900,
            bottom: 1000,
        };
        // caret 完全在工作区外且上方也放不下 → 贴底，窗口完整可见
        let caret = CaretRect {
            x: 50,
            y: 5000,
            w: 2,
            h: 20,
        };
        let (x, y) = position_in_area(caret, 300, 200, area);
        assert_eq!(x, area.left, "左边界内收");
        assert_eq!(y, area.bottom - 200, "贴工作区底");
        assert!(y >= area.top);
    }

    #[test]
    fn position_clamps_to_area_top_when_caret_fully_above() {
        let area = RECT {
            left: 0,
            top: 100,
            right: 1920,
            bottom: 1040,
        };
        // caret 在工作区上方：贴工作区顶
        let caret = CaretRect {
            x: 100,
            y: -100,
            w: 2,
            h: 20,
        };
        let (_, y) = position_in_area(caret, 200, 100, area);
        assert_eq!(y, area.top);
    }

    #[test]
    fn position_without_caret_height_keeps_small_gap() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let caret = CaretRect {
            x: 100,
            y: 100,
            w: 0,
            h: 0,
        };
        let (_, y) = position_in_area(caret, 200, 100, area);
        assert_eq!(y, caret.y + 2, "无 caret 高度时留 2px 间隙");
    }
}
