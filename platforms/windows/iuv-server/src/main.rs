//! iuv-server：全系统一个的引擎服务进程（49 §2/§5 P3a）。
//!
//! 装配（与 TSF engine_host 同源）：词库 + 配置 + 用户库 + 简繁转换表 → `Engine`
//! → `EngineService` → transport 服务。词典缺失/损坏 → 退出非零（服务端没有
//! 「透明模式」的意义——客户端对连不上的服务端本就降级透明）。
//!
//! 用法：`iuv-server [--pipe <name>]`（默认 `iuv.service.v1`）。

// 服务进程无控制台：计划任务/自启拉起时不弹黑窗（日志全落 %TEMP%\iuv-server.log）。
#![windows_subsystem = "windows"]

use std::sync::Arc;

use iuv_core::{paths::iuv_dir, Config, Engine};
use iuv_proto::{Auth, BuildId, Caps};
use iuv_win::logger::log_line;
use iuv_win::transport::{load_or_create_token, TransportServer, SERVICE_PIPE_NAME};

const DICT_FILENAME: &str = "iuv.imedic";
const USERDICT_FILENAME: &str = "iuv.user.imedic";
const OPENCC_FILENAME: &str = "iuv.opencc";

fn main() {
    iuv_win::logger::init_logger("iuv-server.log", true);
    // panic 留痕（2026-10-02 品质审查 S1）：windows_subsystem="windows" 下 panic
    // 输出无控制台可见，钩子落 iuv-server.log（daemon::log::install_panic_hook
    // 此前定义后从未调用）。
    iuv_server::daemon::log::install_panic_hook();
    // 单实例守卫：部署（计划任务 Start-ScheduledTask）与客户端首连拉起
    //（remote_host::spawn_server_process）在部署/重启瞬间并发，命名管道支持多
    // 实例创建——无守卫会抢出多个 server 瓜分连接（工具栏/引擎分家）。后来者
    // 记日志即退（退出自动释放互斥体；持有者存续期 = 进程生命周期，无需显式
    // Release/Close）。
    unsafe {
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        let _ = CreateMutexW(None, false, windows::core::w!("iuv-server-singleton"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log_line("已有 iuv-server 实例在位 → 本实例退出（单实例守卫）");
            return;
        }
    }
    // P4 服务端自渲染候选窗：PMv2（GetDpiForMonitor 按 caret 所在显示器返回
    // 真 per-monitor DPI；窗口创建前置位，晚于任何窗口创建则无效）。
    // SAFETY: 标准一次性进程属性设置；失败（已设置/不支持）忽略。
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let pipe = pipe_name_from_args().unwrap_or_else(|| SERVICE_PIPE_NAME.to_string());

    let t0 = std::time::Instant::now();
    let Some(engine) = load_engine() else {
        log_line("引擎加载失败 → 服务端退出（客户端将降级透明）");
        std::process::exit(1);
    };
    log_line(&format!(
        "引擎加载完成：{:.0} ms（服务端就绪）",
        t0.elapsed().as_millis()
    ));

    let dir = iuv_dir().unwrap_or_else(|| std::env::temp_dir().join("iuv"));
    let auth: Auth = match load_or_create_token(&dir) {
        Ok(a) => a,
        Err(e) => {
            log_line(&format!("共享密钥装配失败（{}）：{e}", dir.display()));
            std::process::exit(2);
        }
    };

    let service = Arc::new(iuv_server::EngineService::new(engine.clone()));
    // P4 配置热载：后台监视 config.json → 引擎热载 + 纪元自增（会话捎带
    // Push::ConfigChanged 通知客户端）；改配置不再需要重启 server。
    // 同线程兼停机哨兵（server.stop）：优雅停机广播 Push::Shutdown 后退出。
    iuv_server::config_watch::spawn(
        engine.clone(),
        service.config_epoch(),
        service.senders_handle(),
    );

    // ---- ② daemon UI 迁入：daemon 状态/工具栏（含桌宠、全局热键、设置页）----
    let daemon_config = iuv_server::daemon::config::load_config();
    iuv_win::logger::set_log_modules_disabled(&daemon_config.disabled_log_modules);
    let upath = dir.join("iuv.user.imedic");
    let dict = iuv_data::UserDict::load(&upath).unwrap_or_else(|e| {
        log_line(&format!("[main] 用户库加载失败（按空库启动）: {e}"));
        iuv_data::UserDict::empty()
    });
    // SHM 写者归 EngineService（②迁移：daemon 副本写者退役 → shm=None）。
    let state = iuv_server::daemon::state::DaemonState::new(dict, None, daemon_config, upath);
    let pet_art = std::sync::Arc::new(iuv_server::daemon::pet_assets::load_pet_art());
    let toolbar = iuv_server::daemon::toolbar::ToolbarHost::spawn(
        state.clone(),
        pet_art,
        std::sync::Arc::new(iuv_server::TransportCtlDispatcher {
            senders: service.senders_handle(),
        }),
    );
    service.attach_ui(state.clone(), toolbar.clone());

    let server = match TransportServer::start(
        iuv_win::transport::ServerConfig {
            pipe_name: pipe.clone(),
            auth,
            caps: Caps(Caps::UIELEMENT),
            build: BuildId(env!("CARGO_PKG_VERSION").to_string()),
            max_connections: 64,
        },
        service,
    ) {
        Ok(s) => s,
        Err(e) => {
            log_line(&format!("transport 服务启动失败（{pipe}）：{e}"));
            std::process::exit(3);
        }
    };
    log_line(&format!(
        "iuv-server 就绪：{pipe}（等待连接；工具栏/设置页已装配）"
    ));
    // 存活到进程结束。**不能写 `let _ = server`**——`_` 模式的临时值在语句结束即析构，
    // TransportServer::drop 会关管道/停 accept（实测：进程活着但管道消失，客户端全放行）。
    let _server = server;
    // ② 主循环（daemon 时代同款）：OpenSettings → 主线程跑 eframe 设置窗；
    // 兜底 flush（设置页清除等非管道路径尽快落盘）。
    loop {
        use std::sync::atomic::Ordering;
        if state.open_settings.swap(false, Ordering::AcqRel) {
            state.settings_open.store(true, Ordering::Release);
            log_line("[main] 收到 OpenSettings，运行设置窗口");
            let _ = iuv_server::daemon::settings::run_settings(&state, &toolbar, &engine);
            state.settings_open.store(false, Ordering::Release);
            // 设置页可能保存了 keymap → 工具条线程全量重注册全局热键（幂等）。
            toolbar.hotkeys_changed();
            log_line("[main] 设置窗口已关闭，继续后台常驻");
            continue;
        }
        state.flush_if_dirty();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `--pipe <name>` 覆盖默认管道名（测试/多实例）。
fn pipe_name_from_args() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--pipe")
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// 引擎装配（与 tsf engine_host 同源：词库/配置/用户库/简繁表；失败仅日志不 panic）。
fn load_engine() -> Option<Arc<Engine>> {
    let dir = iuv_dir().unwrap_or_else(|| std::env::temp_dir().join("iuv"));
    let dict_path = dir.join(DICT_FILENAME);
    let dict = match iuv_data::load(&dict_path) {
        Ok(d) => {
            log_line(&format!(
                "词库加载成功：{}（词条 {}）",
                dict_path.display(),
                d.entry_count()
            ));
            d
        }
        Err(e) => {
            log_line(&format!("词库加载失败：{e}（{}）", dict_path.display()));
            return None;
        }
    };
    let engine = Engine::new(dict, Config::load());
    let user_path = dir.join(USERDICT_FILENAME);
    if let Err(e) = engine.attach_user_dict(user_path.clone()) {
        log_line(&format!("用户词库装配失败（空库继续）：{e}"));
    } else {
        log_line(&format!("用户词库装配成功：{}", user_path.display()));
    }
    let occ_path = dir.join(OPENCC_FILENAME);
    match iuv_data::OpenccTable::load(&occ_path) {
        Ok(t) => {
            engine.attach_script_converter(Some(Arc::new(iuv_core::ScriptConverter::new(t))));
            log_line(&format!("简繁转换器装配成功：{}", occ_path.display()));
        }
        Err(e) => {
            engine.attach_script_converter(None);
            log_line(&format!("简繁转换器装配失败（繁体降级简体）：{e}"));
        }
    }
    Some(engine)
}
