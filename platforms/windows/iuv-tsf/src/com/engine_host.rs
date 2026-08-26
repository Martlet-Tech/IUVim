//! 引擎生命周期宿主（P2.2 从 text_service.rs 拆出）：进程级引擎单例、
//! 后台异步加载、数据文件路径。全模块级函数，不依赖 TextService 实例。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use iuv_core::{Config, Engine};

use crate::log::log_line;

/// 进程级引擎单例（契约 §7：`OnceLock<Arc<Engine>>`）。
/// 词典加载失败 → None = 透明模式（全部按键放行，绝不卡用户）。
static ENGINE: OnceLock<Option<Arc<Engine>>> = OnceLock::new();
/// 加载是否已启动（防重复 spawn；Activate 与 engine() 兜底并发安全）。
static ENGINE_LOAD_STARTED: AtomicBool = AtomicBool::new(false);

/// 取引擎单例（非阻塞）。透明模式 / 加载未完成时返回 None（按键放行）。
pub(crate) fn engine() -> Option<&'static Arc<Engine>> {
    let loaded = ENGINE.get().and_then(|e| e.as_ref());
    if loaded.is_none() && ENGINE.get().is_none() {
        // 兜底：未走 Activate 就被按键（极端路径），触发后台加载。
        start_engine_load();
    }
    loaded
}

/// 后台异步加载引擎：词库 17MB/65 万词条，首键同步加载会卡顿。
/// Activate（切到输入法）时调用；加载中按键 = 透明放行，绝不阻塞按键路径。
/// 加载失败 → set(None) = 永久透明模式（与现状语义一致，不重试）。
pub(crate) fn start_engine_load() {
    if ENGINE_LOAD_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let t0 = std::time::Instant::now();
        log_line("引擎加载开始（后台线程）");
        let engine = load_engine();
        log_line(&format!(
            "引擎加载完成：耗时 {:.0} ms，结果 {:?}",
            t0.elapsed().as_millis(),
            engine.as_ref().map(|_| "就绪").unwrap_or("失败→透明模式")
        ));
        let _ = ENGINE.set(engine);
    });
}

/// 引擎后台加载是否仍在进行（DllCanUnloadNow 用：加载线程运行中访问 DLL 代码，
/// 不可卸载）。set 完成（含失败 set(None)）后恒 false。
pub(crate) fn engine_loading() -> bool {
    ENGINE_LOAD_STARTED.load(Ordering::SeqCst) && ENGINE.get().is_none()
}

fn load_engine() -> Option<Arc<Engine>> {
    let path = dict_path();
    match iuv_data::load(&path) {
        Ok(dict) => {
            log_line(&format!(
                "引擎加载成功：{}（词条 {}）",
                path.display(),
                dict.entry_count()
            ));
            let engine = Engine::new(dict, Config::load());
            // 39-rime-pipeline.md Step3：config.engine == "rime" → 挂载 rime 核心
            // （词库 Arc 共享，M2 调权/屏蔽跨核心同源；切换需重载输入法生效）。
            if engine.config().engine == iuv_core::config::EngineChoice::Rime {
                let rime = iuv_core::RimeEngine::new(engine.shared_dict(), &engine.config());
                engine.attach_core(rime);
                log_line("候选核心：rime（39-rime-pipeline 过渡开关）");
            }
            // M6 日志模块禁用集装配（26-log-modules.md）：引擎配置即共享 config.json，
            // 首装配与 config_epoch 热载（apply_config_hot_reload）两处同步。
            crate::log::set_log_modules_disabled(&engine.config().disabled_log_modules);
            // M2 主动调权用户库装配（18-m2-user-dict.md）：缺失/损坏 → 空库继续，
            // attach 返回 Err 仅记日志（不代表未装配——路径已记录，首次交换时创建文件）。
            let user_path = user_dict_path();
            if let Err(e) = engine.attach_user_dict(user_path.clone()) {
                log_line(&format!("用户词库装配失败（空库继续，路径已记录）：{}", e));
            } else {
                log_line(&format!("用户词库装配成功：{}", user_path.display()));
            }
            // M2.5 简→繁转换器装配（31-script-traditional.md）：iuv.opencc 缺失/损坏 →
            // None 降级简体输出（不阻断）。数据与词库独立装配。
            let occ_path = script_path();
            match iuv_data::OpenccTable::load(&occ_path) {
                Ok(t) => {
                    let conv = iuv_core::ScriptConverter::new(t);
                    engine.attach_script_converter(Some(std::sync::Arc::new(conv)));
                    log_line(&format!(
                        "简繁转换器装配成功：{}（{} 词条）",
                        occ_path.display(),
                        engine.script_converter().map(|c| c.entry_count()).unwrap_or(0)
                    ));
                }
                Err(e) => {
                    engine.attach_script_converter(None);
                    log_line(&format!(
                        "简繁转换器装配失败（繁体模式降级简体输出）：{}",
                        e
                    ));
                }
            }
            Some(engine)
        }
        Err(e) => {
            log_line(&format!(
                "引擎加载失败：{e}（{}），进入透明模式",
                path.display()
            ));
            None
        }
    }
}

/// %LOCALAPPDATA%\iuv\iuv.imedic（用户级数据，契约 §7）。
fn dict_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        std::env::var("APPDATA")
            .map(|a| {
                PathBuf::from(a)
                    .join("Local")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|_| "C:\\Users\\Default\\AppData\\Local".to_owned())
    });
    PathBuf::from(base)
        .join("iuv")
        .join(crate::registration::DICT_FILENAME)
}

/// %LOCALAPPDATA%\iuv\iuv.user.imedic（M2 用户权重覆盖表，与基本库同目录）。
pub(crate) fn user_dict_path() -> PathBuf {
    let mut p = dict_path();
    p.set_file_name("iuv.user.imedic");
    p
}

/// %LOCALAPPDATA%\iuv\iuv.opencc（31-script-traditional.md 简繁转换表，与基本库同目录）。
fn script_path() -> PathBuf {
    let mut p = dict_path();
    p.set_file_name("iuv.opencc");
    p
}