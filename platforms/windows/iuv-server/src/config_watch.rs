//! 配置热载（49 §5 P4「配置热载改服务端持有」）。
//!
//! 服务端引擎配置原为启动时一次性 `Config::load()`，daemon 设置页改配置后
//! 必须重启 iuv-server 才生效（P3 过渡期已知限制）。本模块在**后台线程**监视
//! config.json（mtime+长度，500ms 周期），变化 → `Engine::set_config` 热载 +
//! 配置纪元自增（[`crate::EngineService`] 在请求路径捎带 `Push::ConfigChanged`
//! 通知客户端刷新副本）。
//!
//! 49 §4.7 纪律：磁盘 IO 不进热路径——stat/load 都在本线程，热路径只剩
//! 一次原子读。轮询（而非 ReadDirectoryChangesW）是刻意取舍：单文件 stat
//! 微秒级、每 500ms 一次，复杂度收益比不划算。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use iuv_win::logger::log_line;
use iuv_win::transport::ConnSender;

use iuv_core::Engine;

use iuv_proto::Push;

/// 监视周期。
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 优雅停机哨兵文件名（`iuv_dir()` 下）。部署/卸载脚本创建它请求停机，
/// 本进程检测到即广播 `Push::Shutdown` 后退出——替代 taskkill 强杀（客户端
/// 立即知情透明，不再等管道断裂才发现；文件不存在时哨兵检测零开销）。
const STOP_SENTINEL: &str = "server.stop";

/// 停机哨兵路径（与 main/脚本约定的 `%LOCALAPPDATA%\iuv\server.stop`）。
pub fn stop_sentinel_path() -> Option<PathBuf> {
    iuv_core::paths::iuv_dir().map(|d| d.join(STOP_SENTINEL))
}

/// 监视状态快照（文件标识对：mtime + 长度。原子 rename 保存下两者同变）。
#[derive(Default, PartialEq, Eq)]
struct FileStamp {
    mtime: Option<SystemTime>,
    len: u64,
}

impl FileStamp {
    fn probe(path: &std::path::Path) -> FileStamp {
        match std::fs::metadata(path) {
            Ok(m) => FileStamp {
                mtime: m.modified().ok(),
                len: m.len(),
            },
            Err(_) => FileStamp::default(),
        }
    }
}

/// 启动配置监视 + 停机哨兵线程（常驻；随进程生命周期，无停机通道——main park 永驻形态）。
pub fn spawn(
    engine: Arc<Engine>,
    config_epoch: Arc<AtomicU32>,
    senders: Arc<Mutex<HashMap<(u32, u32), ConnSender>>>,
) {
    let path = iuv_core::config::default_config_path();
    std::thread::Builder::new()
        .name("iuv-config-watch".into())
        .spawn(move || run(path, engine, config_epoch, senders))
        .expect("config_watch 线程创建");
}

fn run(
    path: Option<PathBuf>,
    engine: Arc<Engine>,
    config_epoch: Arc<AtomicU32>,
    senders: Arc<Mutex<HashMap<(u32, u32), ConnSender>>>,
) {
    if path.is_none() {
        log_line("[config] 配置目录不可解析 → 配置热载停用（保持启动时配置）");
    }
    let mut last = path.as_deref().map(FileStamp::probe).unwrap_or_default();
    if let Some(p) = &path {
        log_line(&format!("[config] 配置热载监视：{}", p.display()));
    }
    let sentinel = stop_sentinel_path();
    loop {
        std::thread::sleep(POLL_INTERVAL);
        // 优雅停机哨兵：存在即广播 Shutdown → 宽限 → 退出（幂等：先删文件再停，
        // 防止退出慢时重复触发）。
        if let Some(sp) = &sentinel {
            if sp.exists() {
                let _ = std::fs::remove_file(sp);
                graceful_stop(&senders);
            }
        }
        let Some(path) = &path else {
            continue;
        };
        let cur = FileStamp::probe(path);
        if cur == last {
            continue;
        }
        last = cur;
        // 重载（Config::load 失败回默认值，与启动时语义一致）；先写引擎再推纪元，
        // 客户端收到推送后读盘必然已见新配置（保存是原子 rename）。
        let cfg = iuv_core::Config::load();
        engine.set_config(cfg.clone());
        iuv_win::logger::set_log_modules_disabled(&cfg.disabled_log_modules);
        let epoch = config_epoch.fetch_add(1, Ordering::Release) + 1;
        log_line(&format!(
            "[config] 配置热载生效：epoch={} theme={:?} passthrough_apps={}（引擎 set_config；客户端经 Push::ConfigChanged 刷新副本）",
            epoch, cfg.theme, cfg.passthrough_apps.len(),
        ));
    }
}

/// 优雅停机：向全部连接广播 `Push::Shutdown`（客户端立即透明放行，激活兜底重生
/// 已有）→ 留宽限让帧下行 → 进程退出。单实例守卫随进程释放，计划任务可随即
/// 拉起新版（部署流程：哨兵 → 等退出 → 换文件 → 启动）。
fn graceful_stop(senders: &Arc<Mutex<HashMap<(u32, u32), ConnSender>>>) {
    const GRACE_MS: u32 = 300;
    log_line("[shutdown] 哨兵文件到位 → 广播 Push::Shutdown 并退出");
    // 广播 + 顺带清理：push 失败 = 死连接（句柄已失效），条目陈旧无害但会
    // 噪音报错（真机实锤「句柄无效」）——retain 一并摘除。
    let (total, alive) = {
        let mut map = senders.lock().unwrap_or_else(|e| e.into_inner());
        let total = map.len();
        map.retain(|_, sender| sender.push(Push::Shutdown { grace_ms: GRACE_MS }).is_ok());
        (total, map.len())
    };
    if alive < total {
        log_line(&format!(
            "[shutdown] 通知 {alive} 条存活连接（另清理 {} 条陈旧条目），宽限 {GRACE_MS} ms 后退出",
            total - alive
        ));
    } else {
        log_line(&format!(
            "[shutdown] 已通知 {alive} 条连接，宽限 {GRACE_MS} ms 后退出"
        ));
    }
    std::thread::sleep(Duration::from_millis(GRACE_MS as u64));
    log_line("[shutdown] iuv-server 退出（优雅停机）");
    std::process::exit(0);
}
