//! 54-2 Step 2 真机探针：不用部署输入法，直接在当前桌面上验证窗口探测。
//!
//! 用法（在 iuvim 仓库根目录）：
//! ```text
//! cargo run -p iuv-win --example probe            # 探测光标处窗口（单次）
//! cargo run -p iuv-win --example probe -- --watch # 持续跟随光标，变化时打印
//! cargo run -p iuv-win --example probe -- --list  # 列出全部候选（z 序自顶向下）
//! cargo run -p iuv-win --example probe -- --at 300,300  # 探测指定屏幕坐标（不动鼠标）
//! ```
//!
//! 验收要点：
//! - 把鼠标移到 BongoCat / Shimeji 身上 → 应打印出宠物窗口（而非其下的窗口）
//! - 移到 MateEngine 的 3D 模型上 → 应命中它**下面**的窗口（覆盖层点击穿透，
//!   这正是鼠标真实语义）；把鼠标移到桌面空白处才会命中覆盖层并标"全屏覆盖"
//! - 自家的取景框/提示条/工具栏不会出现在候选里（按 PID 排除）

use std::thread::sleep;
use std::time::Duration;

use iuv_win::window_probe::{
    list_candidates, monitor_rect_at, probe_at, virtual_desktop_rect, Mountability, WindowProbe,
};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

fn cursor() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    // SAFETY: p 为栈上可写 POINT；GetCursorPos 纯查询。
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some((p.x, p.y))
}

fn mount_label(m: Mountability) -> &'static str {
    match m {
        Mountability::Ok => "可挂载",
        Mountability::FullscreenOverlay => "全屏覆盖 · 不可挂载",
        Mountability::Elevated => "不可挂载（权限不足）",
    }
}

fn describe(p: &WindowProbe) -> String {
    format!(
        "{} · {} · {}×{} @({},{}) · {}",
        p.exe_name(),
        p.class_name,
        p.rect.w,
        p.rect.h,
        p.rect.x,
        p.rect.y,
        mount_label(p.mountability)
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // --at X,Y 指定探测点；否则用光标位置
    let at: Option<(i32, i32)> = args
        .iter()
        .position(|a| a == "--at")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| {
            let (x, y) = s.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        });
    let point = at.or_else(cursor);

    let vd = virtual_desktop_rect();
    println!("虚拟桌面 {}×{} @({},{})", vd.w, vd.h, vd.x, vd.y);
    if let Some((x, y)) = point {
        if let Some(m) = monitor_rect_at(x, y) {
            println!(
                "探测点 ({x},{y}) 所在显示器 {}×{} @({},{})",
                m.w, m.h, m.x, m.y
            );
        }
    }
    println!("---");

    if args.iter().any(|a| a == "--list") {
        let cands = list_candidates();
        println!("候选 {} 个（z 序自顶向下）：", cands.len());
        for (i, c) in cands.iter().enumerate() {
            println!("  {i:>3}. {}", describe(c));
        }
        return;
    }

    if args.iter().any(|a| a == "--at") {
        match point.and_then(|(x, y)| probe_at(x, y).map(|p| (x, y, p))) {
            Some((x, y, p)) => println!("({x},{y}) → {}", describe(&p)),
            None => println!("<无候选>"),
        }
        return;
    }

    if args.iter().any(|a| a == "--watch") {
        println!("跟随光标（Ctrl+C 退出）：");
        let mut last = String::new();
        loop {
            let cur = match cursor().and_then(|(x, y)| probe_at(x, y).map(|p| (x, y, p))) {
                Some((x, y, p)) => format!("({x},{y}) {}", describe(&p)),
                None => {
                    let (x, y) = cursor().unwrap_or((0, 0));
                    format!("({x},{y}) <无候选>")
                }
            };
            if cur != last {
                println!("{cur}");
                last = cur;
            }
            sleep(Duration::from_millis(120));
        }
    }

    match point.and_then(|(x, y)| probe_at(x, y).map(|p| (x, y, p))) {
        Some((x, y, p)) => println!("光标 ({x},{y}) → {}", describe(&p)),
        None => println!("光标处无候选（桌面空白）"),
    }
}
