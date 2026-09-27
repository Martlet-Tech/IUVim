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

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use iuv_win::logger::log_line;

use iuv_core::Engine;

/// 监视周期。
const POLL_INTERVAL: Duration = Duration::from_millis(500);

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

/// 启动配置监视线程（常驻；随进程生命周期，无停机通道——main park 永驻形态）。
pub fn spawn(engine: Arc<Engine>, config_epoch: Arc<AtomicU32>) {
    let path = iuv_core::config::default_config_path();
    std::thread::Builder::new()
        .name("iuv-config-watch".into())
        .spawn(move || run(path, engine, config_epoch))
        .expect("config_watch 线程创建");
}

fn run(path: Option<PathBuf>, engine: Arc<Engine>, config_epoch: Arc<AtomicU32>) {
    let Some(path) = path else {
        log_line("[config] 配置目录不可解析 → 配置热载停用（保持启动时配置）");
        return;
    };
    let mut last = FileStamp::probe(&path);
    log_line(&format!("[config] 配置热载监视：{}", path.display()));
    loop {
        std::thread::sleep(POLL_INTERVAL);
        let cur = FileStamp::probe(&path);
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
