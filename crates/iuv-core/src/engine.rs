//! 引擎资源容器。契约 01-contract.md §4。
//!
//! 本文件只持有与管理资源（词库/配置/切分器/语言模型/用户库/简繁转换器）；
//! 候选生成核心 = rime（`rime::RimeEngine`，39-rime-pipeline.md 收尾后唯一核心），
//! 在构造时内部装配，会话工厂产出的会话直接绑定该核心。

use crate::userdict::{UserRemote, UserState};
use crate::{rime::RimeEngine, session::Session, script::ScriptConverter, Config};
use iuv_data::Dict;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// 引擎：进程级单例，跨线程共享。
pub struct Engine {
    /// 词库（Arc：rime 核心与用户库写入共享同一实例——M2 用户库调权/屏蔽同源）。
    pub(crate) dict: Arc<Dict>,
    /// 配置（Mutex：M6 设置页热载 engine.set_config 需要 &self 内部可变）。
    pub(crate) config: Mutex<Config>,
    /// 用户权重覆盖表状态（M2 主动调权，18-m2-user-dict.md）：路径 + 上次加载 mtime
    /// （会话创建时检测跨进程写入的延迟生效；M6 daemon 模式关闭，见 userdict.rs）。
    pub(crate) user_state: Mutex<UserState>,
    /// 用户库远端写后端（M6 daemon 客户端）。None = 本地写盘（现状/降级）。
    /// apply 返回 false（daemon 离线/拒绝）→ 写路径自动降级本地，绝不挂键。
    pub(crate) user_remote: Mutex<Option<Arc<dyn UserRemote>>>,
    /// 简→繁转换器（31-script-traditional.md）。None = 未装配/数据缺失 → 降级简体输出。
    script: Mutex<Option<Arc<ScriptConverter>>>,
    /// 缓存 `config.page_size.max(1)`（P1.6：热路径每键多次读 page_size，避免整份克隆）。
    page_size: AtomicU32,
    /// 候选生成核心（39-rime-pipeline.md 收尾：rime 为唯一核心，构造时内部装配）。
    ime: Arc<dyn crate::api::ImeEngine>,
}

impl Engine {
    /// 默认装配：rime 核心（Quanpin 切分 + UnigramLm 组句均在核心内部构造）。
    pub fn new(dict: Dict, config: Config) -> Arc<Engine> {
        let page_size = config.page_size.max(1) as u32;
        let dict = Arc::new(dict);
        let ime = RimeEngine::new(dict.clone(), &config);
        Arc::new(Engine {
            dict,
            config: Mutex::new(config),
            user_state: Mutex::new(UserState::default()),
            user_remote: Mutex::new(None),
            script: Mutex::new(None),
            page_size: AtomicU32::new(page_size),
            ime,
        })
    }

    pub fn start_session(self: &Arc<Self>) -> Session {
        self.reload_user_dict();
        let runtime = Arc::new(std::sync::Mutex::new(self.config().initial_state));
        Session::over(self.clone(), self.ime.clone(), runtime)
    }

    /// 注入实例运行时四态开会话（32-status-toolbar.md §5.1）：TSF 每实例持有自己的
    /// `Arc<Mutex<ImeState>>`，会话 live 读；引擎进程级单例共享多实例不受影响。
    pub fn start_session_with_runtime(
        self: &Arc<Self>,
        runtime: Arc<std::sync::Mutex<crate::ImeState>>,
    ) -> Session {
        self.reload_user_dict();
        Session::over(self.clone(), self.ime.clone(), runtime)
    }

    /// 装配简→繁转换器（31-script-traditional.md）。`None` = 数据缺失/不启用 → 繁体模式
    /// 降级简体输出（转换层直接跳过，不 panic、不影响会话）。TSF 侧在 load_engine 装配，
    /// 数据文件独立于词库（互不影响加载路径）。
    pub fn attach_script_converter(&self, conv: Option<Arc<ScriptConverter>>) {
        *self.script.lock().unwrap_or_else(|e| e.into_inner()) = conv;
    }

    /// 当前简→繁转换器（None = 未装配/降级）。
    pub fn script_converter(&self) -> Option<Arc<ScriptConverter>> {
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// M6 配置热载（config_epoch 变化 → 设置页保存后触发）：全量替换引擎配置。
    /// 读取点（page_size/max_candidates/candidate_prefix/candidate_orientation/
    /// passthrough_apps/theme）随 `config()` 新值即时生效；TSF 侧键位 keymap 映射
    /// 装配不在此热切（M7 键位热载范畴，见 22-m6-daemon.md；调用方自行记日志）。
    pub fn set_config(&self, config: Config) {
        self.page_size
            .store(config.page_size.max(1) as u32, Ordering::Relaxed);
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
    }

    /// 当前配置快照（克隆：M6 热载后新值立即可见；读侧不用锁穿透引用）。
    pub fn config(&self) -> Config {
        self.config.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 每页候选数（缓存 `config.page_size.max(1)`：热路径每键多次读取，免整份克隆；
    /// `set_config` 热载时同步刷新）。
    pub fn page_size(&self) -> u32 {
        self.page_size.load(Ordering::Relaxed)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_data::Dict;

    fn dict_of(items: Vec<(&str, &str, u32)>) -> Dict {
        Dict::from_entries(
            items
                .into_iter()
                .map(|(c, w, wt)| (c.into(), w.into(), wt))
                .collect(),
        )
    }

    fn swap_dict() -> Dict {
        dict_of(vec![("de".into(), "的", 100000), ("de".into(), "得", 300)])
    }

    /// set_config：配置热载替换引擎配置（M6 config_epoch 触发路径）。
    #[test]
    fn set_config_updates_engine_config() {
        let e = Engine::new(swap_dict(), Config::default());
        assert_eq!(e.config().page_size, 5);
        let cfg = Config {
            page_size: 7,
            passthrough_apps: vec!["dota2.exe".into()],
            ..Config::default()
        };
        e.set_config(cfg.clone());
        assert_eq!(e.config().page_size, 7);
        assert_eq!(e.config().passthrough_apps, vec!["dota2.exe".to_owned()]);
        assert_eq!(e.config().keymap, cfg.keymap, "未改字段保持新值");
    }
}
