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
mod toolbar;
mod toolbar_icons;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use iuv_data::UserDict;
use iuv_win::{PipeServer, Request, Response, ShmWriter, SignalServer};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use crate::state::DaemonState;
use crate::toolbar::ToolbarHost;

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

    // ---- 0. DPI 感知（32-status-toolbar.md §6 工具栏坐标正确性前提）----
    // 首行声明 per-monitor DPI aware：工具栏窗口在任意 DPI 下拿真实物理坐标/DPI。
    // 否则进程被系统虚拟化为 96dpi 逻辑坐标，且设置页 eframe/winit 会中途把进程切成
    // per-monitor aware → 同一窗口坐标含义突变 → 工具栏位置漂移到屏幕外（2026-08-21
    // 实测 toolbar.json pos=32767,32767）。必须在建任何窗口前调用。
    // SAFETY: SetProcessDpiAwarenessContext 进程级设置；失败（系统策略/已设置）静默。
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // ---- 1. 单实例互斥 ----
    let Some(mutex_daemon) = acquire_mutex(MUTEX_DAEMON) else {
        log::log_line("[main] 已有 iuv-daemon 实例运行，本进程退出");
        return 0;
    };

    // ---- 2. 配置 ----
    let daemon_config = config::load_config();
    // 日志模块禁用集装配（26-log-modules.md）：设置页开发者标签修改后 apply 也会重装配。
    log::set_log_modules_disabled(&daemon_config.disabled_log_modules);
    log::log_line(&format!(
        "[main] 配置：theme={} passthrough={:?} disabled_log={:?}",
        daemon_config.theme, daemon_config.passthrough_apps, daemon_config.disabled_log_modules
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

    // ---- 5. 浮动工具栏宿主（32-status-toolbar.md §6：全局唯一看板，独立消息泵线程）----
    let toolbar = ToolbarHost::spawn(state.clone());

    // ---- 6. 管道监听线程（写请求应用 + publish + 立即写盘）----
    spawn_pipe_thread(state.clone(), toolbar.clone());
    spawn_signal_pipe_thread(toolbar.clone());

    // ---- 7. 主线程：命令轮询 + eframe 设置窗（winit 事件循环必须在主线程）----
    log::log_line("[main] 就绪，主线程进入命令轮询循环（后台常驻，无托盘图标）");
    loop {
        if state.quit_flag.swap(false, Ordering::AcqRel) {
            break;
        }
        if state.open_settings.swap(false, Ordering::AcqRel) {
            state.close_settings.store(false, Ordering::Release);
            // 标记窗口运行中：此后的 OpenSettings 转发聚焦而非积压（防幽灵重开）。
            state.settings_open.store(true, Ordering::Release);
            log::log_line("[main] 收到 OpenSettings，运行设置窗口");
            let _ = settings::run_settings(&state);
            state.settings_open.store(false, Ordering::Release);
            log::log_line("[main] 设置窗口已关闭，继续后台常驻");
            continue;
        }
        // 兜底 flush：任何非管道路径置 dirty（如设置页清除）都尽快落盘，
        // 防注销硬杀时磁盘残留旧库（2026-08-18 实测复活 bug）。dirty 已清时零成本。
        state.flush_if_dirty();
        std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
    }

    // ---- 8. 退出清理 ----
    log::log_line("[main] 收到退出信号，开始清理");
    toolbar.shutdown(); // 停工具条消息泵线程（PostThreadMessage WM_QUIT）
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
/// 命令请求 OpenSettings/Quit 置标志 → 主线程轮询处理；工具栏请求 → toolbar 宿主消费）→ 响应。
/// 阻塞 Accept 在进程退出时随线程终止（无清理路径，可接受）。
fn spawn_pipe_thread(state: Arc<DaemonState>, toolbar: Arc<ToolbarHost>) {
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
            let toolbar = toolbar.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = server.serve(|req| handle_request(&state, &toolbar, req));
            }));
        });
    match spawned {
        Ok(_) => log::log_line("[pipe] 管道监听线程已启动"),
        Err(e) => log::log_line(&format!("[pipe] 启动监听线程失败: {e}")),
    }
}

/// 工具条信号通道（40-toolbar-show-hide-governance.md）：专用管道 accept 循环 +
/// thread-per-connection——每连接独立线程循环读帧喂工具条 FIFO，连接间互不阻塞
/// （数据面单线程串行 serve 的争用问题在此结构性消除）。
fn spawn_signal_pipe_thread(toolbar: Arc<ToolbarHost>) {
    let spawned = std::thread::Builder::new()
        .name("iuv-signal".to_string())
        .spawn(move || loop {
            match SignalServer::accept() {
                Ok(server) => {
                    let toolbar = toolbar.clone();
                    let worker = std::thread::Builder::new()
                        .name("iuv-signal-conn".to_string())
                        .spawn(move || loop {
                            match server.recv() {
                                Ok(sig) => toolbar.handle_signal(&sig),
                                Err(_) => break, // 对端断开 / 帧错 → 结束本连接线程
                            }
                        });
                    if let Err(e) = worker {
                        log::log_line(&format!("[signal] 连接线程启动失败: {e}"));
                    }
                }
                Err(e) => {
                    log::log_line(&format!("[signal] accept 失败（200ms 后重试）: {e}"));
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        });
    match spawned {
        Ok(_) => log::log_line("[signal] 信号通道监听线程已启动"),
        Err(e) => log::log_line(&format!("[signal] 启动监听线程失败: {e}")),
    }
}

/// 处理单个请求。写请求：应用（写时复制）→ publish（bump version）→ 立即写盘；
/// 命令请求：置标志返回（主线程消费），不触碰用户库；工具栏请求（32-status-toolbar.md
/// §4.1 Register/StateSync/Active/Unregister/ToggleToolbar）→ toolbar 宿主消费，不触碰用户库。
fn handle_request(state: &Arc<DaemonState>, toolbar: &Arc<ToolbarHost>, req: &Request) -> Response {
    // 工具栏相关请求先行消费（不触碰用户库）。
    if toolbar.handle_request(req) {
        return Response::Ok {
            version: state.current_version(),
        };
    }
    match req {
        // Ping：健康检查 + 当前 version（不触碰用户库）。
        Request::Ping => {
            return Response::Ok {
                version: state.current_version(),
            }
        }
        // 语言栏菜单打开时查询显隐偏好（菜单项文案「显示/隐藏工具栏」二选一）。
        Request::GetToolbarVisible => {
            return Response::ToolbarVisible {
                visible: toolbar.visible(),
            }
        }
        // 语言栏菜单「设置」：主线程弹 egui 设置窗。窗口已开时直接 Win32 还原/置前
        //（学任务栏 SC_RESTORE 手法）——最小化态 eframe 无帧，egui ViewportCommand
        // 永远执行不到；也不积压 open_settings 标志（否则关窗后幽灵重开，2026-08-22 实测）。
        Request::OpenSettings => {
            if state.settings_open.load(Ordering::Acquire) {
                if crate::settings::focus_existing_window() {
                    log::log_line("[pipe] 设置页已打开 → 已还原/置前");
                } else {
                    log::log_line("[pipe] 设置页已打开 → 窗口未就绪（创建中？），忽略本次聚焦");
                }
            } else {
                log::log_line("[pipe] 收到 OpenSettings 命令");
                state.open_settings.store(true, Ordering::Release);
            }
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