//! 候选窗演示程序（契约 14-mod-iuv-tsf-candwin.md §3）。
//! 运行：`cargo run -p iuv-tsf --example candwin_demo`
//!
//! 人眼验收要点：
//! - 不抢焦点：打开记事本让光标闪烁，候选窗不应夺取键盘焦点；
//! - 无闪烁：翻页/移动过程平滑，无白闪；
//! - 高亮正确：每页高亮行随翻页轮换；
//! - 尺寸自适应：翻页时窗口随内容自动缩放。

use std::time::Duration;

use iuv_core::PageInfo;
use iuv_tsf::ui::{CandidateUi, CaretRect, GdiCandidateWindow, UiSnapshot};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_QUIT,
};

/// 4 页演示数据（reading 固定 `ni'hao`）。
const PAGES: [&[&str]; 4] = [
    &["你好", "泥嚎", "拟好", "逆浩", "匿好"],
    &["你们", "妮们", "拟门", "泥门", "尼们"],
    &["内耗", "内嚎", "呢嚎", "馁号", "肋号"],
    &["那时", "哪时", "那史", "钠石", "拿是"],
];

fn make_snapshot(page: usize) -> UiSnapshot {
    UiSnapshot {
        reading: "ni'hao".to_string(),
        candidates: PAGES[page].iter().map(|s| s.to_string()).collect(),
        selected: page % PAGES[page].len(),
        page: PageInfo {
            page,
            page_count: PAGES.len(),
            page_size: PAGES[page].len(),
            total: PAGES.len() * PAGES[0].len(),
        },
    }
}

/// Esc 是否被按下（不依赖焦点，候选窗本身不抢焦点收不到按键）。
fn esc_pressed() -> bool {
    // SAFETY: GetAsyncKeyState 无副作用；返回值高位为 1 表示按下（i16 为负）
    unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0 }
}

/// 泵线程消息队列（处理候选窗 WM_PAINT 等）。返回 true 表示收到 WM_QUIT。
fn pump_messages() -> bool {
    let mut msg = MSG::default();
    // SAFETY: msg 在 PeekMessageW 调用期间可写；hwnd 过滤为 None = 线程全部消息
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return true;
            }
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
    false
}

fn main() {
    println!("IUV 输入法 候选窗演示 —— 按 Esc 退出（建议同时打开记事本观察不抢焦点）");
    let mut win = GdiCandidateWindow::new();
    let mut tick: u32 = 0;
    while tick < 30 {
        if pump_messages() || esc_pressed() {
            break;
        }
        let page = (tick as usize) % PAGES.len();
        let snap = make_snapshot(page);
        // 光标位置每 tick 移动一次，验证跟随
        let caret = CaretRect {
            x: 120 + (tick as i32 * 37) % 240,
            y: 140 + (tick as i32 * 53) % 220,
            w: 2,
            h: 20,
        };
        if tick == 0 {
            win.show(&snap, caret);
        } else {
            win.update(&snap);
            win.move_to(caret);
        }
        println!(
            "第 {} 页，候选 1 = {}，位置 ({}, {})",
            page + 1,
            snap.candidates[0],
            caret.x,
            caret.y
        );
        tick += 1;
        std::thread::sleep(Duration::from_millis(1500));
    }
    win.hide();
    println!("演示结束");
}
