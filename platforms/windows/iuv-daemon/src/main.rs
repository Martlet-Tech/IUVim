//! iuv 守护进程（M6）：唯一持有用户库 + 共享段发布 + 命名管道 IPC + 托盘 + 设置页。
//!
//! 生命周期（任务书 `docs/plan/22-m6-daemon.md` §4）：
//! 1. 单实例互斥 `Local\iuv-daemon`（已有实例 → 退出）+ 托盘宿主互斥 `Local\iuv-tray-host`；
//! 2. 加载 `%LOCALAPPDATA%\iuv\iuv.user.imedic`（缺失 → 空库）；
//! 3. 建共享段 `ShmWriter::create_or_open` → 初始 publish 一次；
//! 4. 注册托盘 + 起管道监听线程（每请求应用 → publish → 置 dirty）；
//! 5. 主线程 Win32 消息循环（GetMessageW）：WM_TRAY（托盘点击）、WM_TIMER（2s 聚合 flush）；
//! 6. 退出（托盘菜单「退出 iuv」→ PostQuitMessage）：关设置窗 → 强写盘 → 移除托盘 → 释放互斥。
//!
//! 绝不 panic：`main` 顶层 `catch_unwind` + panic 钩子落日志；管道/设置线程体自包
//! `catch_unwind`。守护进程崩溃会丢用户库状态，故一切错误降级而非 panic。

mod config;
mod log;
mod settings;
mod state;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use eframe::egui;
use iuv_data::{PipeServer, Request, Response, ShmWriter, UserDict};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};

use crate::state::DaemonState;

/// 用户库文件名（%LOCALAPPDATA%\iuv\iuv.user.imedic）。
const USER_DICT_FILENAME: &str = "iuv.user.imedic";
/// 单实例互斥（已存在活跃实例 → 本进程退出）。
const MUTEX_DAEMON: &str = "Local\\iuv-daemon";
/// 托盘宿主互斥（与 M5 一致；本进程持有期间会话进程不托管托盘）。
const MUTEX_TRAY_HOST: &str = "Local\\iuv-tray-host";
/// flush 聚合周期（毫秒）。
const FLUSH_INTERVAL_MS: u32 = 2000;

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
    // 托盘宿主互斥：失败（被占用）不阻塞启动（仅托盘归谁的问题由守护进程单实例已保证）。
    let mutex_tray = acquire_mutex(MUTEX_TRAY_HOST);

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

    // ---- 5. 托盘 ----
    let tray_hwnd = tray::install(state.clone());

    // ---- 6. 管道监听线程 ----
    spawn_pipe_thread(state.clone());

    // ---- 7. flush 定时器（2s 聚合写盘；托盘窗口为空也照常跑）----
    if let Some(hwnd) = tray_hwnd {
        // SAFETY: 与托盘窗口关联的 2s 定时器；回调 = WM_TIMER → tray_wnd_proc flush。
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetTimer(
                Some(hwnd),
                tray::TIMER_FLUSH,
                FLUSH_INTERVAL_MS,
                None,
            );
        }
    }

    // ---- 8. 主线程消息循环 ----
    log::log_line("[main] 就绪，进入消息循环（托盘 + flush）");
    let mut msg = MSG::default();
    // SAFETY: GetMessageW 阻塞取消息；WM_QUIT → FALSE 退出循环。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // SAFETY: Translate+Dispatch 把消息路由到托盘/菜单窗口过程（均主线程创建）。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // ---- 9. 退出清理 ----
    log::log_line("[main] 消息循环退出，开始清理");
    // 关设置窗口（信号 → 设置线程 Close → run_native 返回）。
    state.close_settings.store(true, Ordering::Release);
    if let Some(ctx) = state
        .settings_ctx
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
    {
        let _ = ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    // 强写盘（未 flush 时退出）。
    state.flush_now();
    // 移除托盘。
    if let Some(hwnd) = tray_hwnd {
        tray::remove_icon(hwnd);
    }
    // 释放互斥（Drop 关闭句柄 = 释放所有权，后续实例可接管）。HANDLE 为 Copy，
    // 显式 drop 无副作用——`let _ =` 仅为语义清晰。
    let _ = mutex_tray;
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
    let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) }
        .ok()?;
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

/// 管道监听线程：循环 Accept → 应用请求（写时复制）→ publish（bump version + dirty）→
/// 响应。阻塞 Accept 在进程退出时随线程终止（无清理路径，可接受）。
fn spawn_pipe_thread(state: Arc<DaemonState>) {
    let spawned = std::thread::Builder::new()
        .name("iuv-pipe".to_string())
        .spawn(move || {
            loop {
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
            }
        });
    match spawned {
        Ok(_) => log::log_line("[pipe] 管道监听线程已启动"),
        Err(e) => log::log_line(&format!("[pipe] 启动监听线程失败: {e}")),
    }
}

/// 应用单个写请求 → 返回响应（含共享段新 version）。
fn handle_request(state: &Arc<DaemonState>, req: &Request) -> Response {
    // Ping：健康检查 + 当前 version（不触碰用户库）。
    if matches!(req, Request::Ping) {
        return Response::Ok {
            version: state.current_version(),
        };
    }
    // 应用（写时复制；改完即释放锁，再 publish）。
    {
        let mut dict = state.dict.lock().unwrap_or_else(|p| p.into_inner());
        let d = match req {
            Request::Swap {
                a_code,
                a_word,
                a_adj,
                b_code,
                b_word,
                b_adj,
            } => dict.apply_swap(a_code, a_word, *a_adj, b_code, b_word, *b_adj),
            Request::Set { code, word, adj } => dict.set_entry(code, word, *adj),
            Request::Remove { code, word } => dict.remove_entry(code, word),
            Request::Block { code, word } => dict.block(code, word),
            Request::Ping => unreachable!("上方已处理"),
        };
        *dict = d;
    }
    // 发布（写共享段 + bump version + dirty）。
    let version = state.publish().unwrap_or(0);
    log::log_line(&format!("[pipe] 已应用请求 {req:?} → version={version}"));
    Response::Ok { version }
}