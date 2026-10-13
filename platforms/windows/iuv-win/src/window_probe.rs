//! 目标窗口探测（54-2 Step 2）：枚举 + 过滤 + 自算 z 序命中 + 可挂性判定。
//!
//! ## 为什么不用 `WindowFromPoint`
//! 它会**跳过 `WS_EX_TRANSPARENT` 窗口**——MateEngine 那类全屏透明覆盖层因此永远
//! 选不中（截图工具 PixPin 实测同一条 API 语义，同样抓不到）。本模块改用
//! `EnumWindows` 自算命中，于是覆盖层也能被选中并标记"不可挂载"。
//!
//! ## 命中规则（两遍扫描，见 `pick_hit`）
//! ① 先取最上层**非点击穿透**窗口——鼠标真正会命中的那个，与截图工具同语义；
//! ② ①无命中时才取最上层含 `WS_EX_TRANSPARENT` 的窗口（覆盖层类）。
//! 若只有一遍（纯 z 序），覆盖层会吞掉整个桌面，用户永远指不到下面的东西。
//!
//! ## 排除项
//! 本进程窗口（取景框/描边/提示条/工具栏/候选窗都是自家的，否则会「自己高亮自己」）、
//! 桌面/壁纸层、不可见窗口、最小化窗口、退化矩形。
//!
//! 全部公开函数不 panic：查询失败返回 None/空串，静默降级。

use std::mem::size_of;

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetShellWindow, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    GWL_EXSTYLE, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

/// 屏幕矩形（物理像素；左上闭右下开）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl ProbeRect {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// 目标可挂性——捕捉期即判定，别等贴附完才发现锚点是一整块屏幕。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mountability {
    /// 可挂载
    Ok,
    /// 全屏覆盖层（`LAYERED|TRANSPARENT` 且盖满显示器）：窗口不是宠物，挂载语义不成立
    FullscreenOverlay,
    /// 权限不足（提权/受保护进程）：拿不到进程信息，建不了档案
    Elevated,
}

/// 候选目标窗口快照（供提示条展示与建档案）。
///
/// `hwnd` 存 **isize** 而非 `HWND`：`HWND` 含裸指针、`!Send`，而捕捉结果要跨线程
/// （捕捉线程 → 消费方）回传。消费方按需 `HWND(hwnd as *mut _)` 还原。
#[derive(Clone)]
pub struct WindowProbe {
    pub hwnd: isize,
    pub class_name: String,
    pub process_path: String,
    pub title: String,
    pub rect: ProbeRect,
    pub ex_style: u32,
    pub mountability: Mountability,
}

impl WindowProbe {
    /// 进程可执行文件名（档案匹配与提示条用；路径拿不到时为空串）。
    pub fn exe_name(&self) -> &str {
        self.process_path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&self.process_path)
    }

    /// 是否点击穿透（`WS_EX_TRANSPARENT`）——贴附后点击会落到桌面，交互需另设计。
    pub fn click_through(&self) -> bool {
        self.ex_style & WS_EX_TRANSPARENT.0 != 0
    }
}

// ============================================================
// 纯判定（可单测）
// ============================================================

/// 矩形是否含点（左上闭右下开）。
fn rect_contains(r: ProbeRect, x: i32, y: i32) -> bool {
    r.w > 0 && r.h > 0 && x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

/// 是否全屏覆盖层：盖满显示器 **且** 分层或穿透。
/// 这类目标"窗口不是宠物"（MateEngine 实测 1532×900 覆盖主屏），挂载锚点会退化成
/// 整块屏幕 —— 捕捉期就判出来，当场提示回退。
fn is_fullscreen_overlay(win: ProbeRect, mon: ProbeRect, ex_style: u32) -> bool {
    if mon.w <= 0 || mon.h <= 0 || win.w <= 0 || win.h <= 0 {
        return false;
    }
    let layered = ex_style & (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) != 0;
    layered
        && win.x <= mon.x
        && win.y <= mon.y
        && win.right() >= mon.right()
        && win.bottom() >= mon.bottom()
}

/// 枚举收集的原始候选（Win32 侧填充；选择逻辑为纯函数以便单测）。
#[derive(Clone, Copy)]
struct Cand {
    hwnd: HWND,
    rect: ProbeRect,
    click_through: bool,
}

/// 命中选择（**纯函数**）：`EnumWindows` 按 z 序自顶向下枚举，故「第一个含点者」
/// 即最上层。两遍扫描——先非穿透，后穿透（理由见模块头）。
fn pick_hit(cands: &[Cand], x: i32, y: i32) -> Option<usize> {
    for (i, c) in cands.iter().enumerate() {
        if !c.click_through && rect_contains(c.rect, x, y) {
            return Some(i);
        }
    }
    for (i, c) in cands.iter().enumerate() {
        if c.click_through && rect_contains(c.rect, x, y) {
            return Some(i);
        }
    }
    None
}

// ============================================================
// Win32 查询
// ============================================================

/// 是否桌面/壁纸层窗口。**必须排除**：桌面矩形恒等于整屏，不排除会永远命中它，
/// 用户指哪都是"桌面"。
/// 注：`fullscreen.rs` 的全屏抑制用同一规则（同一份实现，防两处规则漂移）。
pub fn is_desktop_window(hwnd: HWND) -> bool {
    // SAFETY: GetShellWindow / GetClassNameW 纯查询。
    let shell = unsafe { GetShellWindow() };
    if !shell.is_invalid() && shell == hwnd {
        return true;
    }
    matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW")
}

/// 窗口类名（失败返回空串）。
fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: buf 为栈上可写缓冲，长度由数组保证。
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    if n == 0 || n > buf.len() {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n])
}

/// 窗口标题（失败返回空串）。
fn window_title(hwnd: HWND) -> String {
    // SAFETY: GetWindowTextLengthW 纯查询。
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: buf 长度 = len + 1，足够容纳标题与结尾 NUL。
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    if n == 0 || n > buf.len() {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n])
}

/// 窗口矩形（失败/退化返回 None）。
fn window_rect(hwnd: HWND) -> Option<ProbeRect> {
    let mut r = windows::Win32::Foundation::RECT::default();
    // SAFETY: r 为栈上可写 RECT。
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return None; // 最小化窗口等退化矩形
    }
    Some(ProbeRect {
        x: r.left,
        y: r.top,
        w,
        h,
    })
}

/// 窗口扩展样式（失败 0）。
fn ex_style(hwnd: HWND) -> u32 {
    // SAFETY: GetWindowLongPtrW(GWL_EXSTYLE) 纯查询。
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 }
}

/// 窗口属主进程 pid（失败 0）。
fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: pid 为栈上可写；TSF/枚举线程均可查询。
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// 进程完整路径（拿不到返回空串）。用 `PROCESS_QUERY_LIMITED_INFORMATION`——
/// 该访问权对跨完整性进程普遍可用，能拿到就说明能建档案。
fn process_path(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    // SAFETY: OpenProcess 纯查询；成功则下方配对 CloseHandle。
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return String::new();
    };
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    // SAFETY: buf 长度已传入 len；PWSTR 指向栈上可写缓冲。
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    // SAFETY: 与上方 OpenProcess 配对。
    let _ = unsafe { CloseHandle(handle) };
    if !ok || len == 0 || len as usize > buf.len() {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

/// 是否提权/受保护进程——**判据 = 进程路径拿不到**。
///
/// 刻意**不用** `OpenProcess(PROCESS_QUERY_INFORMATION)` 试开判提权：那个信号依赖
/// **调用方自身的完整性级别**（本仓沙箱里跑探针实测：同一进程对 javaw 可开、对
/// bongocat-app 不可开 → 误报"权限不足"），且提权本身**并不阻塞** MVP 路径
/// （读位置 `GetWindowRect`、`SetWinEventHook` 都不过 UIPI；UIPI 拦的是跨完整性
/// 窗口消息，而我们不做 `SetWindowPos`/`SendMessage` 驱动宠物）。
/// 真正卡住的是**建档案**——路径都拿不到就没法按档案找回目标。
fn is_unreadable(process_path: &str) -> bool {
    process_path.is_empty()
}

/// 枚举收集候选（可见、非最小化、非本进程、非桌面、矩形有效）。
fn collect_candidates() -> Vec<Cand> {
    // SAFETY: out 在本调用栈内存活；回调仅在此期间被调用。
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam 由下方调用点传入 &mut Vec<Cand>，生命周期覆盖枚举全程。
        let out = unsafe { &mut *(lparam.0 as *mut Vec<Cand>) };
        // SAFETY: 以下均为纯查询。
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
            return BOOL(1);
        }
        if window_pid(hwnd) == crate::logger::process_id() {
            return BOOL(1); // 自家窗口（取景框/描边/提示条/工具栏/候选窗）
        }
        if is_desktop_window(hwnd) {
            return BOOL(1);
        }
        let Some(rect) = window_rect(hwnd) else {
            return BOOL(1);
        };
        let ex = ex_style(hwnd);
        out.push(Cand {
            hwnd,
            rect,
            click_through: ex & WS_EX_TRANSPARENT.0 != 0,
        });
        BOOL(1)
    }

    let mut out: Vec<Cand> = Vec::new();
    // SAFETY: EnumWindows 同步枚举；回调内只做纯查询与 push。
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut out as *mut Vec<Cand> as isize)) };
    out
}

/// 由 hwnd 组装完整快照（含可挂性判定）。
fn build_probe(hwnd: HWND, rect: ProbeRect, ex: u32) -> WindowProbe {
    let pid = window_pid(hwnd);
    let path = process_path(pid);
    let (cx, cy) = rect.center();
    let monitor = monitor_rect_at(cx, cy).unwrap_or(ProbeRect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    });
    let mountability = if is_unreadable(&path) {
        Mountability::Elevated
    } else if is_fullscreen_overlay(rect, monitor, ex) {
        Mountability::FullscreenOverlay
    } else {
        Mountability::Ok
    };
    WindowProbe {
        hwnd: hwnd.0 as isize,
        class_name: class_name(hwnd),
        process_path: path,
        title: window_title(hwnd),
        rect,
        ex_style: ex,
        mountability,
    }
}

// ============================================================
// 公开 API
// ============================================================

/// 命中检测：点 `(x, y)`（屏幕物理像素）处的目标窗口。
/// 返回 None = 无候选（桌面空白处且无覆盖层）。规则见模块头。
pub fn probe_at(x: i32, y: i32) -> Option<WindowProbe> {
    let cands = collect_candidates();
    let idx = pick_hit(&cands, x, y)?;
    let c = cands[idx];
    Some(build_probe(c.hwnd, c.rect, ex_style(c.hwnd)))
}

/// 枚举全部候选（调试/列表选择用；顺序 = z 序自顶向下）。
pub fn list_candidates() -> Vec<WindowProbe> {
    collect_candidates()
        .into_iter()
        .map(|c| build_probe(c.hwnd, c.rect, ex_style(c.hwnd)))
        .collect()
}

/// 点 `(x, y)` 所在显示器矩形（物理像素，**含**任务栏区域——取景框要的是屏幕边界，
/// 不是工作区）。
pub fn monitor_rect_at(x: i32, y: i32) -> Option<ProbeRect> {
    // SAFETY: MonitorFromPoint 纯查询；DEFAULTTONEAREST 保证返回有效句柄。
    let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info 已初始化且存活；rcMonitor = 含任务栏的整块显示器。
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let r = info.rcMonitor;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(ProbeRect {
        x: r.left,
        y: r.top,
        w,
        h,
    })
}

/// 虚拟桌面矩形（所有显示器并集，物理像素）——跟随夹紧用。
pub fn virtual_desktop_rect() -> ProbeRect {
    // SAFETY: GetSystemMetrics 纯查询。
    unsafe {
        ProbeRect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(x: i32, y: i32, w: i32, h: i32, click_through: bool) -> Cand {
        Cand {
            hwnd: HWND::default(),
            rect: ProbeRect { x, y, w, h },
            click_through,
        }
    }

    #[test]
    fn rect_contains_is_half_open() {
        let r = ProbeRect {
            x: 10,
            y: 20,
            w: 100,
            h: 50,
        };
        assert!(rect_contains(r, 10, 20), "左上角含");
        assert!(rect_contains(r, 109, 69), "右下角内侧含");
        assert!(!rect_contains(r, 110, 70), "右下角外侧不含（半开）");
        assert!(!rect_contains(r, 9, 20), "左外侧不含");
        // 退化矩形恒不含
        assert!(!rect_contains(
            ProbeRect {
                x: 0,
                y: 0,
                w: 0,
                h: 10
            },
            0,
            0
        ));
    }

    #[test]
    fn hit_prefers_non_click_through_over_overlay() {
        // 枚举顺序 = z 序自顶向下：覆盖层在最上，普通窗口在其下
        let cands = vec![
            cand(0, 0, 1920, 1080, true),    // 全屏覆盖层（MateEngine 那种）
            cand(100, 100, 400, 300, false), // 普通窗口
        ];
        // 点在普通窗口范围内 → 必须命中普通窗口，不被覆盖层吞掉
        assert_eq!(pick_hit(&cands, 200, 200), Some(1), "优先非穿透窗口");
        // 点在普通窗口之外（覆盖层范围内）→ 才轮到覆盖层
        assert_eq!(pick_hit(&cands, 50, 50), Some(0), "无普通窗口时才取覆盖层");
        // 点在任何窗口之外
        assert_eq!(pick_hit(&cands, -10, -10), None);
    }

    #[test]
    fn hit_takes_topmost_within_same_class() {
        // 两个普通窗口重叠：先枚举者（更上层）胜出
        let cands = vec![cand(0, 0, 500, 500, false), cand(100, 100, 500, 500, false)];
        assert_eq!(pick_hit(&cands, 200, 200), Some(0), "最上层胜出");
        assert_eq!(pick_hit(&cands, 50, 50), Some(0), "只有上层含点");
        // 550 落在下层的延伸区（上层只到 500）→ 只有下层含点
        assert_eq!(pick_hit(&cands, 550, 550), Some(1), "只有下层含点");
    }

    #[test]
    fn fullscreen_overlay_needs_layered_and_full_cover() {
        let mon = ProbeRect {
            x: 0,
            y: 0,
            w: 1600,
            h: 900,
        };
        // MateEngine 实测形态：1532x900 略小于显示器？——不，覆盖判定用 >=；
        // 这里用恰好盖满 + LAYERED 的形态
        let cover = ProbeRect {
            x: 0,
            y: 0,
            w: 1600,
            h: 900,
        };
        assert!(is_fullscreen_overlay(cover, mon, WS_EX_LAYERED.0));
        assert!(is_fullscreen_overlay(cover, mon, WS_EX_TRANSPARENT.0));
        // 盖满但**没分层** → 是普通全屏窗口，不算"覆盖层"（挂载语义仍成立）
        assert!(!is_fullscreen_overlay(cover, mon, 0));
        // 分层但没盖满（普通宠物窗）→ 不算
        let pet = ProbeRect {
            x: 100,
            y: 100,
            w: 350,
            h: 203,
        };
        assert!(!is_fullscreen_overlay(pet, mon, WS_EX_LAYERED.0));
        // 超出显示器（负起点）也算覆盖
        let over = ProbeRect {
            x: -4,
            y: -4,
            w: 1608,
            h: 908,
        };
        assert!(is_fullscreen_overlay(over, mon, WS_EX_LAYERED.0));
        // 退化显示器矩形 → 不猜
        assert!(!is_fullscreen_overlay(
            cover,
            ProbeRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0
            },
            WS_EX_LAYERED.0
        ));
    }

    #[test]
    fn probe_rect_helpers() {
        let r = ProbeRect {
            x: 1920,
            y: 0,
            w: 1280,
            h: 900,
        };
        assert_eq!(r.right(), 3200);
        assert_eq!(r.bottom(), 900);
        assert_eq!(r.center(), (2560, 450));
    }
}
