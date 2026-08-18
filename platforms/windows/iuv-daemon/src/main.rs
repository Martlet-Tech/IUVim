//! iuv 守护进程（M6）：唯一持有用户库 + 共享段发布 + 命名管道 IPC + 设置页。
//!
//! 生命周期（任务书 `docs/plan/22-m6-daemon.md` §4）：
//! 1. 单实例互斥 `Local\iuv-daemon`（已有实例 → 退出）；
//! 2. 加载 `%LOCALAPPDATA%\iuv\iuv.user.imedic`（缺失 → 空库）；
//! 3. 建共享段 `ShmWriter::create_or_open` → 初始 publish 一次；
//! 4. 起管道监听线程（每请求应用 → publish → 立即写盘）；
//! 5. 主线程轮询命令标志：`OpenSettings`（语言栏菜单「设置」）→ 主线程跑
//!    eframe 设置窗（winit 事件循环只能在主线程）；`Quit`（卸载脚本/调试）→ 退出。
//! 6. 退出：强写盘 → 释放互斥。托盘已移除（2026-08-17 用户决策：无独立托盘图标，
//!    入口全部走语言栏「中/英」按钮右键菜单，见 `21-m5-tray-menu.md`）。
//!
//! 绝不 panic：`main` 顶层 `catch_unwind` + panic 钩子落日志；管道线程体自包
//! `catch_unwind`。守护进程崩溃会丢用户库状态，故一切错误降级而非 panic。

mod config;
mod log;
mod settings;
mod state;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use iuv_data::{PipeServer, Request, Response, ShmWriter, UserDict};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

use crate::state::DaemonState;

/// 用户库文件名（%LOCALAPPDATA%\iuv\iuv.user.imedic）。
const USER_DICT_FILENAME: &str = "iuv.user.imedic";
/// 单实例互斥（已存在活跃实例 → 本进程退出）。
const MUTEX_DAEMON: &str = "Local\\iuv-daemon";
/// 命令轮询间隔（毫秒）。
const POLL_INTERVAL_MS: u64 = 50;

fn main() {
    log::install_panic_hook();
    let exit_code = std::panic::catch_unwind(run).unwrap_or(1);
    log::log_line(&format!("[main] 退出码 {exit_code}"));
    std::process::exit(exit_code);
}

/// 守护进程主体（返回退出码）。
fn run() -> i32 {
    log::log_line("======== iuv-daemon 启动 ========");

    // ---- 1. 单实例互斥 ----
    let Some(mutex_daemon) = acquire_mutex(MUTEX_DAEMON) else {
        log::log_line("[main] 已有 iuv-daemon 实例运行，本进程退出");
        return 0;
    };

    // ---- 2. 配置 ----
    let daemon_config = config::load_config();
    log::log_line(&format!(
        "[main] 配置：theme={} passthrough={:?}",
        daemon_config.theme, daemon_config.passthrough_apps
    ));

    // ---- 3. 用户库 ----
    let Some(upath) = user_dict_path() else {
        log::log_line("[main] 无法定位用户库路径（无 LOCALAPPDATA/TEMP），退出");
        return 1;
    };
    let dict = UserDict::load(&upath).unwrap_or_else(|e| {
        log::log_line(&format!("[main] 用户库加载失败（按空库启动）: {e}"));
        UserDict::empty()
    });
    log::log_line(&format!(
        "[main] 用户库已加载：覆盖 {} 条、屏蔽 {} 条",
        dict.cover_count(),
        dict.block_count()
    ));

    // ---- 4. 共享段 ----
    let shm = match ShmWriter::create_or_open() {
        Ok(w) => {
            log::log_line("[main] 共享段就绪");
            Some(w)
        }
        Err(e) => {
            log::log_line(&format!(
                "[main] 共享段创建失败（会话进程将降级自读文件）: {e}"
            ));
            None
        }
    };
    let state = DaemonState::new(dict, shm, daemon_config, upath.clone());
    state.publish();

    // ---- 5. 管道监听线程（写请求应用 + publish + 立即写盘）----
    spawn_pipe_thread(state.clone());

    // ---- 6. 主线程：命令轮询 + eframe 设置窗（winit 事件循环必须在主线程）----
    log::log_line("[main] 就绪，主线程进入命令轮询循环（后台常驻，无托盘图标）");
    loop {
        if state.quit_flag.swap(false, Ordering::AcqRel) {
            break;
        }
        if state.open_settings.swap(false, Ordering::AcqRel) {
            state.close_settings.store(false, Ordering::Release);
            log::log_line("[main] 收到 OpenSettings，运行设置窗口");
            let _ = settings::run_settings(&state);
            log::log_line("[main] 设置窗口已关闭，继续后台常驻");
            continue;
        }
        // 兜底 flush：任何非管道路径置 dirty（如设置页清除）都尽快落盘，
        // 防注销硬杀时磁盘残留旧库（2026-08-18 实测复活 bug）。dirty 已清时零成本。
        state.flush_if_dirty();
        std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
    }

    // ---- 7. 退出清理 ----
    log::log_line("[main] 收到退出信号，开始清理");
    state.close_settings.store(true, Ordering::Release);
    state.flush_now();
    let _ = mutex_daemon;
    log::log_line("======== iuv-daemon 退出 ========");
    0
}

/// 用户库文件路径：%LOCALAPPDATA%\iuv\iuv.user.imedic。
fn user_dict_path() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .or_else(|| std::env::var("APPDATA").ok().map(|a| format!("{a}\\Local")))
        .or_else(|| std::env::var("HOME").ok())?;
    Some(PathBuf::from(base).join("iuv").join(USER_DICT_FILENAME))
}

/// 获取具名互斥（bInitialOwner=true）：已存在且非废弃（另一实例活跃）→ None。
/// 废弃（原持有者崩溃释放）→ 本进程接管。
fn acquire_mutex(name: &str) -> Option<HANDLE> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: wide 以 NUL 结尾；bInitialOwner=true 便于检测废弃。
    let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) }.ok()?;
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if !already {
        return Some(handle);
    }
    // 已存在：0ms 探测是否可接管（废弃 → WAIT_ABANDONED；已释放 → WAIT_OBJECT_0）。
    // SAFETY: 0ms 轮询不阻塞。
    let r = unsafe { WaitForSingleObject(handle, 0) };
    if r == WAIT_ABANDONED || r == WAIT_OBJECT_0 {
        Some(handle)
    } else {
        // SAFETY: 无法接管，关闭句柄（不持有）。
        let _ = unsafe { CloseHandle(handle) };
        None
    }
}

/// 管道监听线程：循环 Accept → 处理请求（写请求应用 → publish → 立即写盘；
/// 命令请求 OpenSettings/Quit 置标志 → 主线程轮询处理）→ 响应。
/// 阻塞 Accept 在进程退出时随线程终止（无清理路径，可接受）。
fn spawn_pipe_thread(state: Arc<DaemonState>) {
    let spawned = std::thread::Builder::new()
        .name("iuv-pipe".to_string())
        .spawn(move || loop {
            let server = match PipeServer::accept() {
                Ok(s) => s,
                Err(e) => {
                    log::log_line(&format!("[pipe] accept 失败（200ms 后重试）: {e}"));
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
            };
            let state = state.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = server.serve(|req| handle_request(&state, req));
            }));
        });
    match spawned {
        Ok(_) => log::log_line("[pipe] 管道监听线程已启动"),
        Err(e) => log::log_line(&format!("[pipe] 启动监听线程失败: {e}")),
    }
}

/// 处理单个请求。写请求：应用（写时复制）→ publish（bump version）→ 立即写盘；
/// 命令请求：置标志返回（主线程消费），不触碰用户库。
fn handle_request(state: &Arc<DaemonState>, req: &Request) -> Response {
    match req {
        // Ping：健康检查 + 当前 version（不触碰用户库）。
        Request::Ping => {
            return Response::Ok {
                version: state.current_version(),
            }
        }
        // 语言栏菜单「设置」：主线程弹 egui 设置窗。
        Request::OpenSettings => {
            log::log_line("[pipe] 收到 OpenSettings 命令");
            state.open_settings.store(true, Ordering::Release);
            return Response::Ok {
                version: state.current_version(),
            };
        }
        // 退出（卸载脚本/调试）：主线程退出循环；设置窗开着则一并关闭。
        Request::Quit => {
            log::log_line("[pipe] 收到 Quit 命令");
            state.quit_flag.store(true, Ordering::Release);
            state.close_settings.store(true, Ordering::Release);
            return Response::Ok {
                version: state.current_version(),
            };
        }
        _ => {}
    }
    // 写请求：应用（写时复制；改完即释放锁，再 publish）。
    {
        let mut dict = state.dict.lock().unwrap_or_else(|p| p.into_inner());
        let d = match req {
            // M2 调权互写语义（18-m2-user-dict.md）：a/b 互写对方合成权重。
            // 请求携带 swap 前的旧权重（a_adj/b_adj），必须交叉传入（a←b_adj, b←a_adj），
            // 与引擎本地 apply_swap(a, b_eff, b, a_eff) 一致——原实现各写各的导致
            // "的"恒被写回旧权重（2026-08-17 实测修复，见 daemon 日志 a_adj 恒 5）。
            Request::Swap {
                a_code,
                a_word,
                a_adj,
                b_code,
                b_word,
                b_adj,
            } => dict.apply_swap(a_code, a_word, *b_adj, b_code, b_word, *a_adj),
            Request::Set { code, word, adj } => dict.set_entry(code, word, *adj),
            Request::Remove { code, word } => dict.remove_entry(code, word),
            Request::Block { code, word } => dict.block(code, word),
            _ => unreachable!("上方已处理命令/Ping"),
        };
        *dict = d;
    }
    // 发布（写共享段 + bump version）→ 立即写盘（用户库小，替代 2s 聚合定时器）。
    let version = state.publish().unwrap_or(0);
    state.flush_now();
    log::log_line(&format!("[pipe] 已应用请求 {req:?} → version={version}"));
    Response::Ok { version }
}