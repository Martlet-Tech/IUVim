//! iuv-server：全系统一个的引擎服务进程（49 §2/§5 P3a）。
//!
//! 装配（与 TSF engine_host 同源）：词库 + 配置 + 用户库 + 简繁转换表 → `Engine`
//! → `EngineService` → transport 服务。词典缺失/损坏 → 退出非零（服务端没有
//! 「透明模式」的意义——客户端对连不上的服务端本就降级透明）。
//!
//! 用法：`iuv-server [--pipe <name>]`（默认 `iuv.service.v1`）。

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

    let service = Arc::new(iuv_server::EngineService::new(engine));
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
    log_line(&format!("iuv-server 就绪：{pipe}（等待连接）"));
    // 存活到进程结束。**不能写 `let _ = server`**——`_` 模式的临时值在语句结束即析构，
    // TransportServer::drop 会关管道/停 accept（实测：进程活着但管道消失，客户端全放行）。
    let _server = server;
    loop {
        std::thread::park();
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
