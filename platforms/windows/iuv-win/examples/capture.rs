//! 54-2 Step 3 真机验证：**不用部署**，直接跑捕捉态 UI 看手感。
//!
//! ```text
//! cargo run -p iuv-win --example capture
//! ```
//!
//! 行为：屏幕上出现光标所在显示器四角的取景框 → 鼠标悬停到任意窗口，
//! 该窗口被描边 + 旁边跟一条提示条（进程 · 窗口类 · 尺寸 · 可挂性）。
//! **左键确认**（打印结果）、**右键或 Esc 取消**。
//!
//! 验收要点：
//! - 悬停 BongoCat / Shimeji → 描边贴住宠物窗、提示条写"可挂载"
//! - 悬停 MateEngine 的 3D 模型 → 命中它**下面**的窗口（覆盖层点击穿透，鼠标真实语义）；
//!   鼠标移到桌面空白处才会命中覆盖层并写"全屏覆盖 · 不可挂载"（警示色描边）
//! - 三件装饰件不出现在自己的候选里（按 PID 排除），描边不会"套娃"
//! - 不抢焦点：捕捉期间在别处打字不受影响

use iuv_ui::theme_light;
use iuv_win::finder::{self, CaptureOutcome};
use iuv_win::window_probe::Mountability;

fn main() {
    println!("进入捕捉态……（左键确认 / 右键或 Esc 取消）");
    // start 异步返回（捕捉跑在独立线程），故用通道把结果带回 main 并在此阻塞等待。
    let (tx, rx) = std::sync::mpsc::channel();
    let ok = finder::start(theme_light(), move |outcome| {
        let _ = tx.send(outcome);
    });
    if !ok {
        println!("启动失败（可能已在捕捉中）");
        return;
    }
    match rx.recv() {
        Ok(CaptureOutcome::Picked(p)) => {
            let m = match p.mountability {
                Mountability::Ok => "可挂载",
                Mountability::FullscreenOverlay => "全屏覆盖 · 不可挂载",
                Mountability::Elevated => "不可挂载（权限不足）",
            };
            println!("\n选中：{}", p.process_path);
            println!("  窗口类   : {}", p.class_name);
            println!("  标题     : {}", p.title);
            println!(
                "  矩形     : {}×{} @({},{})",
                p.rect.w, p.rect.h, p.rect.x, p.rect.y
            );
            println!("  exStyle  : 0x{:08X}", p.ex_style);
            println!("  点击穿透 : {}", p.click_through());
            println!("  可挂性   : {m}");
        }
        Ok(CaptureOutcome::Cancelled) => println!("\n已取消"),
        Err(_) => println!("捕捉线程异常结束"),
    }
}
