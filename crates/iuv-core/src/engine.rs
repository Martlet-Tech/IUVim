//! 引擎：候选生成。契约 01-contract.md §4 engine.rs / §4.2 算法。

use crate::routes::Route;
use crate::userdict::{UserRemote, UserState};
use crate::{
    schema::Quanpin, session::Session, script::ScriptConverter, Config, InputSchema, LmProvider,
    UnigramLm,
};
use iuv_data::Dict;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// 引擎：进程级单例，跨线程共享。
pub struct Engine {
    pub(crate) dict: Dict,
    /// 配置（Mutex：M6 设置页热载 engine.set_config 需要 &self 内部可变）。
    pub(crate) config: Mutex<Config>,
    pub(crate) schema: Box<dyn InputSchema>,
    pub(crate) lm: Box<dyn LmProvider>,
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
}

impl Engine {
    /// 默认装配：Quanpin + UnigramLm。
    pub fn new(dict: Dict, config: Config) -> Arc<Engine> {
        let syllables = dict.syllables().clone();
        let lm = UnigramLm::new(dict.total_weight(), dict.entry_count());
        Self::with_parts(
            dict,
            config,
            Box::new(Quanpin::new(syllables)),
            Box::new(lm),
        )
    }

    /// 全注入构造器（测试与后续里程碑用）。
    pub fn with_parts(
        dict: Dict,
        config: Config,
        schema: Box<dyn InputSchema>,
        lm: Box<dyn LmProvider>,
    ) -> Arc<Engine> {
        let page_size = config.page_size.max(1) as u32;
        Arc::new(Engine {
            dict,
            config: Mutex::new(config),
            schema,
            lm,
            user_state: Mutex::new(UserState::default()),
            user_remote: Mutex::new(None),
            script: Mutex::new(None),
            page_size: AtomicU32::new(page_size),
        })
    }

    pub fn start_session(self: &Arc<Self>) -> Session {
        self.reload_user_dict();
        Session::new(self.clone())
    }

    /// 注入实例运行时四态开会话（32-status-toolbar.md §5.1）：TSF 每实例持有自己的
    /// `Arc<Mutex<RuntimeState>>`，会话 live 读；引擎进程级单例共享多实例不受影响。
    pub fn start_session_with_runtime(
        self: &Arc<Self>,
        runtime: Arc<std::sync::Mutex<crate::RuntimeState>>,
    ) -> Session {
        self.reload_user_dict();
        Session::with_runtime(self.clone(), runtime)
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

    /// 档位判定（唯一判定点）：`plain` 为去 `'` 的整串，`plans` 为全部切分方案
    /// （消费端遍历所有方案，2026-08-14：多段判定不再只看贪心方案[0]）。
    /// 与契约 §4.2 路由表逐行对应：
    /// - 整串是音节真前缀 → 单字档（切分器可能把它切成多段，但微软实测只出单字）
    /// - 单段完整音节：无替代切分 → 单字档；有替代切分（xian）→ 全拼（混排西安）
    /// - 单段非前缀（i/u/v）→ 空
    /// - 多段：**存在任一全完整方案 → 全拼**（dier 贪心 [die,r] 是 Mixed，但 [di,er]
    ///   全完整应走全拼枚举——否则「第二」不可达，实测 2026-08-14）；**末音节可补全
    ///   （除末段外全完整 + 末段为音节前缀）→ 也归全拼 2b**（shigechengy → 补 y）；
    ///   否则按段完整性分派（全不完整 → 简拼；混合 → 混拼，仅中段不完整如 nhao）
    fn classify(&self, plain: &str, seg: &[String], plans: &[Vec<String>]) -> Route {
        if !plain.is_empty() && self.is_syllable_prefix(plain) && !self.is_syllable(plain) {
            return Route::PrefixChars;
        }
        if seg.len() == 1 {
            if self.is_syllable(&seg[0]) {
                return if plans.len() > 1 {
                    Route::AmbiguousSyllable
                } else {
                    Route::CompleteChars
                };
            }
            return Route::Empty;
        }
        let any_full = plans
            .iter()
            .any(|p| p.iter().all(|s| !s.is_empty() && self.is_syllable(s)));
        // 末音节可补全（2026-08-18）：除末段外全为完整音节 + 末段为音节前缀
        // （`shigechengy` → 末段 `y`，可补 yu/yi/yang/…）→ 归入全拼档走 2b 整句通道，
        // 词条通道按输入砍（砍 `y` 而非补 `yu`），与 2a 路径砍完第一刀后前缀对齐。
        // 否则 `shigechengy` 会被误判 Mixed（扩展笛卡尔）——2b 场景丢失。
        let tail_completable = seg.len() >= 2
            && seg[..seg.len() - 1]
                .iter()
                .all(|s| !s.is_empty() && self.is_syllable(s))
            && {
                let last = seg.last().unwrap();
                !last.is_empty() && !self.is_syllable(last) && self.is_syllable_prefix(last)
            };
        if any_full || tail_completable {
            Route::FullPinyin
        } else if seg.iter().all(|s| !s.is_empty() && !self.is_syllable(s)) {
            Route::Abbrev
        } else {
            Route::Mixed
        }
    }

    /// 消费端方案重排（2026-08-14）：方案[0] = 词频最优而非贪心——分节显示与主路径
    /// 跟随用户最可能打的词（keneng → ke'neng 可能；dier → di'er 第二）。
    /// 排序键 = 方案 join 键 exact 词条最大权重（词条优先；无词条 = 0），
    /// 稳定排序保贪心原序。切分函数零改动（消费端遍历所有方案）。
    pub(crate) fn rank_plans(&self, plans: Vec<Vec<String>>) -> Vec<Vec<String>> {
        if plans.len() <= 1 {
            return plans;
        }
        let mut scored: Vec<(u32, usize)> = plans
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let key = p.join("'");
                let w = self.dict.exact(&key).first().map(|e| e.weight).unwrap_or(0);
                (w, i)
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| plans[i].clone()).collect()
    }

    /// 按契约 §4.2 生成候选。
    ///
    /// 路由（M1.5，微软实测对齐，见 docs/research/msime-probe-checklist.txt）：
    /// - 整串为音节前缀（`c`/`sh`/`zho`）→ 纯单字（单字桶，词频序）
    /// - 完整单音节：无歧义（`shi`）→ 纯单字（exact 全量）；歧义（`xian`→[xi,an]）→ 全拼两通道
    /// - 单段非前缀（`i`/`u`/`v`）→ 无候选（末尾兜底：原文候选，见 generate_candidates）
    /// - 多段全完整（`nihao`/`xi'an`）**或末音节可补全**（`shigechengy`）→ 全拼两通道
    ///   （整句通道唯一一次 Viterbi 2a/2b + 词条通道砍尾巴 exact）
    /// - 多段全不完整（`nh`/`nhm`/`nhmsx`）→ 简拼键逐级砍尾巴（构建期键，O(1) exact）
    /// - 多段混合（`nhao`，中段不完整）→ 不完整段展开音节配对查询（上限内，超限降级）
    ///
    /// 部分消费：候选 seg_len=k，选中间级词经 session 悬空续接把尾巴重建为组合。
    pub(crate) fn generate_candidates(
        &self,
        raw: &str,
        seg: &[String],
        plans: &[Vec<String>],
    ) -> Vec<crate::Candidate> {
        let plain = crate::strip_apostrophes(raw);
        let mut cands = match self.classify(&plain, seg, plans) {
            Route::PrefixChars => self.single_segment_candidates(&plain),
            Route::CompleteChars => self.single_segment_candidates(&seg[0]),
            Route::AmbiguousSyllable | Route::FullPinyin => {
                self.full_pinyin_candidates(&raw, &plain, seg)
            }
            Route::Abbrev => self.abbrev_candidates(seg),
            Route::Mixed => self.mixed_candidates(seg),
            Route::Empty => Vec::new(),
        };

        // 前缀补全（联想）：默认关闭（微软化，候选仅 exact）；config 开启时追加。
        //    用方案[0] 的 `'` 键做前缀匹配（词库键已分隔化）。
        //    联想词消费全部当前段（seg_len = n），选中即整词上屏。
        // 配置快照：单点锁取克隆（热载 set_config 与读取并发安全）。
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if cfg.candidate_prefix {
            let n = seg.len();
            let squashed = seg.join("'");
            for e in &self.dict.prefix(&squashed, 20) {
                let kind = crate::CandidateKind::for_word(&e.word);
                cands.push(crate::Candidate::for_entry(e, kind, n));
            }
        }

        // 按 text 去重（保序，先见先留）
        let mut seen = std::collections::HashSet::new();
        cands.retain(|c| seen.insert(c.text.clone()));

        // 截断到 max_candidates
        cands.truncate(cfg.max_candidates);

        // 兜底：所有路由均无候选且输入非空 → 原文候选（"不认识"语义：`input`/`window`/`i`
        // 等无法命中任何词库的输入，窗口内容不空、可 1/Space 直接上屏原文）。
        // 复用现有 Word/Char 惯例（多字符 Word、单字符 Char），不新增候选类型；
        // 无编号呈现由 UI 按 text == 预编辑原文 判定。seg_len = 段数 → 全消费、会话结束。
        if cands.is_empty() && !plain.is_empty() {
            let kind = if plain.chars().count() >= 2 {
                crate::CandidateKind::Word
            } else {
                crate::CandidateKind::Char
            };
            cands.push(crate::Candidate::new(
                plain.clone(),
                kind,
                plain.clone(),
                0,
                seg.len(),
            ));
        }
        cands
    }

    pub(crate) fn is_syllable(&self, s: &str) -> bool {
        self.dict.syllables().contains(s)
    }

    fn is_syllable_prefix(&self, s: &str) -> bool {
        self.dict.syllables().iter().any(|syl| syl.starts_with(s))
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