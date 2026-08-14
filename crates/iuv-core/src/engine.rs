//! 引擎：候选生成。契约 01-contract.md §4 engine.rs / §4.2 算法。

use crate::{
    rerank::RerankCtx, schema::Quanpin, session::Session, store::NullStore, Config, InputSchema,
    LmProvider, RerankStage, UnigramLm, UserDataStore,
};
use iuv_data::{Dict, Entry, UserDict};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// 引擎：进程级单例，跨线程共享。
pub struct Engine {
    pub(crate) dict: Dict,
    pub(crate) config: Config,
    pub(crate) schema: Box<dyn InputSchema>,
    pub(crate) lm: Box<dyn LmProvider>,
    pub(crate) stages: Vec<Box<dyn RerankStage>>,
    pub(crate) store: Mutex<Box<dyn UserDataStore>>,
    /// 用户权重覆盖表状态（M2 主动调权，18-m2-user-dict.md）：路径 + 上次加载 mtime
    /// （会话创建时检测跨进程写入的延迟生效）。
    user_state: Mutex<UserState>,
}

/// 用户库装配状态（不可变路径 + 可变 mtime 基线）。
#[derive(Default)]
struct UserState {
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
}

/// 候选生成档位（契约 §4.2 路由表的一等概念）：输入经 `Engine::classify` 归入
/// 微软实测的 5 档 + 空档，`generate_candidates` 按档位分派，后续加档（M3 模糊音
/// 等）= 加一个 Route 臂，不再往分派函数里叠 if。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// 严格前缀（`c`/`sh`/`zho`）→ 纯单字（单字桶）
    PrefixChars,
    /// 完整单音节无歧义（`shi`/`de`/`ba`）→ 纯单字（exact 全量）
    CompleteChars,
    /// 完整单音节有歧义（`xian`→[xi,an]）→ 全拼 k-loop（替代切分词混排）
    AmbiguousSyllable,
    /// 多段全完整（`nihao`/`xi'an`）→ 全拼 k-loop
    FullPinyin,
    /// 多段全不完整（`nh`/`nhm`）→ 简拼键逐级砍尾巴
    Abbrev,
    /// 多段混合（`nhao`）→ 简拼段展开配对
    Mixed,
    /// 单段非前缀（`i`/`u`/`v`）或空输入 → 无候选（生成末尾有原文兜底，见 generate_candidates）
    Empty,
}

impl Engine {
    /// 默认装配：Quanpin + UnigramLm + [StaticOrder] + NullStore。
    pub fn new(dict: Dict, config: Config) -> Arc<Engine> {
        let syllables = dict.syllables().clone();
        let lm = UnigramLm::new(dict.total_weight(), dict.entry_count());
        Self::with_parts(
            dict,
            config,
            Box::new(Quanpin::new(syllables)),
            Box::new(lm),
            vec![Box::new(crate::rerank::StaticOrder)],
            Box::new(NullStore),
        )
    }

    /// 全注入构造器（测试与后续里程碑用）。
    pub fn with_parts(
        dict: Dict,
        config: Config,
        schema: Box<dyn InputSchema>,
        lm: Box<dyn LmProvider>,
        stages: Vec<Box<dyn RerankStage>>,
        store: Box<dyn UserDataStore>,
    ) -> Arc<Engine> {
        Arc::new(Engine {
            dict,
            config,
            schema,
            lm,
            stages,
            store: Mutex::new(store),
            user_state: Mutex::new(UserState::default()),
        })
    }

    pub fn start_session(self: &Arc<Self>) -> Session {
        self.reload_user_dict();
        Session::new(self.clone())
    }

    /// 装配用户权重覆盖表（M2 主动调权）。
    /// **任何失败都降级为空库继续**（用户库不允许阻断输入法）：缺失/损坏 → 空
    /// UserDict + 路径照常记录（后续 swap 写盘时创建/重建文件）。返回 `Err` 仅
    /// 供调用方记日志，**不代表未装配**。
    pub fn attach_user_dict(&self, path: PathBuf) -> std::io::Result<()> {
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        let (user, err) = match UserDict::load(&path) {
            Ok(u) => (u, None),
            Err(e) => (UserDict::empty(), Some(e)),
        };
        self.dict.set_user(Arc::new(user));
        *self.user_state.lock().unwrap_or_else(|e| e.into_inner()) = UserState {
            path: Some(path),
            mtime,
        };
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// 用户库 mtime 检测重载（跨进程延迟生效：其他进程写盘后，本进程新会话拿到新库）。
    /// 文件变化 → 重载成功替换；失败（删除窗口/损坏）→ 保持旧库。
    fn reload_user_dict(&self) {
        let state = self.user_state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(path) = state.path.clone() else {
            return;
        };
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime == state.mtime {
            return;
        }
        drop(state);
        if let Ok(user) = UserDict::load(&path) {
            self.dict.set_user(Arc::new(user));
        }
        *self.user_state.lock().unwrap_or_else(|e| e.into_inner()) = UserState {
            path: Some(path),
            mtime,
        };
    }

    /// M2 主动调权：a/b 两词**交换有效权重**（绝对值覆盖，互写对方合成权重）。
    /// 双 code：候选页内相邻词可能跨 code（单段档桶候选 sha/shi…同属 `sh`）。
    /// 任一不在基本库（如整句候选）→ 忽略（防御即可）。
    /// 写盘失败不阻断：内存态已生效（本次会话立即重排），持久化下次调整时重试。
    pub(crate) fn swap_weights(&self, a_code: &str, a_word: &str, b_code: &str, b_word: &str) {
        let Some(a_base) = self
            .dict
            .exact(a_code)
            .into_iter()
            .find(|e| e.word == a_word)
            .map(|e| e.weight)
        else {
            return;
        };
        let Some(b_base) = self
            .dict
            .exact(b_code)
            .into_iter()
            .find(|e| e.word == b_word)
            .map(|e| e.weight)
        else {
            return;
        };
        let user = self.dict.user();
        let eff = |code: &str, word: &str, base: u32| -> u32 {
            user.as_ref()
                .and_then(|u| {
                    u.adjusted(code)
                        .iter()
                        .find(|(w, _)| w == word)
                        .map(|(_, a)| *a)
                })
                .unwrap_or(base)
        };
        let a_eff = eff(a_code, a_word, a_base);
        let b_eff = eff(b_code, b_word, b_base);
        let next = if let Some(u) = user.as_deref() {
            u.apply_swap(a_code, a_word, b_eff, b_code, b_word, a_eff)
        } else {
            UserDict::empty().apply_swap(a_code, a_word, b_eff, b_code, b_word, a_eff)
        };
        self.install_user(next);
    }

    /// 替换用户库内存态并持久化（写盘失败不阻断：内存态已生效，下次调整重试）。
    /// mtime 基线同步刷新（防止本进程下次会话对自写文件无谓重载）。
    fn install_user(&self, next: UserDict) {
        let mut state = self.user_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = &state.path {
            if next.save(path).is_ok() {
                state.mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
            }
        }
        drop(state);
        self.dict.set_user(Arc::new(next));
    }

    /// M2 自造词记录（逐字选择 commit 时调用，18-m2-user-dict.md）：场景 0/a/b 权重判定。
    /// - 0：词库（含用户库）已有整词 → 跳过（幂等：重复自造被拦截，权重不漂移）
    /// - a：无命中 → 常量 `PHRASE_DEFAULT_WEIGHT`
    /// - b：n 条命中 → 目标位 = 首页最后一位：n ≥ page_size → avg(cand[ps-2], cand[ps-1])
    ///   （u64 计算防溢出）；n < page_size → cand[n-1] − 1（saturating 防 0 下溢）
    pub(crate) fn record_phrase(&self, code: &str, text: &str) {
        const PHRASE_DEFAULT_WEIGHT: u32 = 8000;
        let entries = self.dict.exact(code); // 叠加视图（含用户库独有条目）
        if entries.iter().any(|e| e.word == text) {
            return; // 场景 0
        }
        let ps = self.config.page_size.max(1);
        let n = entries.len();
        let w = if n == 0 {
            PHRASE_DEFAULT_WEIGHT
        } else if n < ps {
            entries[n - 1].weight.saturating_sub(1)
        } else {
            ((entries[ps - 2].weight as u64 + entries[ps - 1].weight as u64) / 2) as u32
        };
        let next = match self.dict.user() {
            Some(u) => u.set_entry(code, text, w),
            None => UserDict::empty().set_entry(code, text, w),
        };
        self.install_user(next);
    }

    /// M2 隐藏候选（Shift+Delete）：先删用户库条目（自造词/覆盖），
    /// 否则屏蔽基础库词条。写盘失败不阻断（内存态已生效）。
    pub(crate) fn hide_entry(&self, code: &str, text: &str) {
        let next = match self.dict.user() {
            Some(u) if u.adjusted(code).iter().any(|(w, _)| w == text) => {
                u.remove_entry(code, text)
            }
            Some(u) => u.block(code, text),
            None => UserDict::empty().block(code, text),
        };
        self.install_user(next);
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 调试/REPL 用精确查询。
    pub fn lookup(&self, squashed_code: &str) -> Vec<Entry> {
        self.dict.exact(squashed_code)
    }

    /// 档位判定（唯一判定点）：`plain` 为去 `'` 的整串，`plans` 为全部切分方案
    /// （消费端遍历所有方案，2026-08-14：多段判定不再只看贪心方案[0]）。
    /// 与契约 §4.2 路由表逐行对应：
    /// - 整串是音节真前缀 → 单字档（切分器可能把它切成多段，但微软实测只出单字）
    /// - 单段完整音节：无替代切分 → 单字档；有替代切分（xian）→ 全拼（混排西安）
    /// - 单段非前缀（i/u/v）→ 空
    /// - 多段：**存在任一全完整方案 → 全拼**（dier 贪心 [die,r] 是 Mixed，但 [di,er]
    ///   全完整应走全拼枚举——否则「第二」不可达，实测 2026-08-14）；否则按段完整性
    ///   分派（全不完整 → 简拼；混合 → 混拼）
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
        if any_full {
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
    /// - 完整单音节：无歧义（`shi`）→ 纯单字（exact 全量）；歧义（`xian`→[xi,an]）→ 全拼 k-loop
    /// - 单段非前缀（`i`/`u`/`v`）→ 无候选（末尾兜底：原文候选，见 generate_candidates）
    /// - 多段全完整（`nihao`/`xi'an`）→ 全拼 k-loop（viterbi 组句 + 逐级枚举）
    /// - 多段全不完整（`nh`/`nhm`/`nhmsx`）→ 简拼键逐级砍尾巴（构建期键，O(1) exact）
    /// - 多段混合（`nhao`）→ 不完整段展开音节配对查询（上限内，超限降级）
    ///
    /// 部分消费：候选 seg_len=k，选中间级词经 session 悬空续接把尾巴重建为组合。
    pub(crate) fn generate_candidates(
        &self,
        raw: &str,
        seg: &[String],
        plans: &[Vec<String>],
    ) -> Vec<crate::Candidate> {
        let plain: String = raw.chars().filter(|c| *c != '\'').collect();
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
        if self.config.candidate_prefix {
            let n = seg.len();
            let squashed = seg.join("'");
            for e in &self.dict.prefix(&squashed, 20) {
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    n,
                ));
            }
        }

        // 按 text 去重（保序，先见先留）
        let mut seen = std::collections::HashSet::new();
        cands.retain(|c| seen.insert(c.text.clone()));

        // 截断到 max_candidates
        cands.truncate(self.config.max_candidates);

        // 依次过 stages 管线
        let now = SystemTime::now();
        let store = self.store.lock().expect("store lock poisoned");
        let ctx = RerankCtx {
            raw,
            seg,
            store: store.as_ref(),
            config: &self.config,
            now,
        };
        for stage in &self.stages {
            stage.rerank(&ctx, &mut cands);
        }

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

    /// 单段档：完整音节或音节前缀 → 纯单字（词频序）。空档（i/u/v）由 classify 拦截。
    /// 微软实测：单段输入无论完整与否只出单字（shi→是时十使，无"时间/时候"）。
    /// 数据源分两路（M1.5 修正）：完整音节 → `dict.exact_single` 全部同音字（首字母桶
    /// 混收多字词会把同音字挤出 top-N，如 shi 只剩 5 字）；严格前缀 → 单字桶
    /// （桶只收单字，前缀无法 exact）。
    /// 候选**全量返回不截断**（微软对齐：sh 候选 600+ 全给、翻页可达，见
    /// docs/research/msime-probe-checklist.txt G3），由全局 max_candidates 兜底。
    fn single_segment_candidates(&self, s: &str) -> Vec<crate::Candidate> {
        if s.is_empty() {
            return Vec::new();
        }
        let entries: Vec<iuv_data::Entry> = if self.is_syllable(s) {
            self.dict.exact_single(s)
        } else {
            // 严格前缀：单字桶（桶只收单字，过滤 starts_with）
            let first = s.chars().next().unwrap();
            self.dict
                .initial_top(first, iuv_data::INITIAL_BUCKET_SIZE)
                .into_iter()
                .filter(|e| e.code.starts_with(s))
                .collect()
        };
        entries
            .into_iter()
            .map(|e| crate::Candidate::new(e.word, crate::CandidateKind::Char, e.code, e.weight, 1))
            .collect()
    }

    /// 简拼键档：多段全不完整（`nh`/`nhm`/`nhmsx`）→ 构建期简拼键逐级砍尾巴。
    /// 每级 k：exact(前 k 段首字母串)；尾巴段由 session 悬空续接重建为组合。
    /// 微软实测：简拼只出词（纯 exact 匹配，无单字、无更长词前缀）。
    fn abbrev_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
        const PER_LEVEL_EXACT: usize = 20;
        let n = seg.len();
        let mut cands = Vec::new();
        for k in (1..=n).rev() {
            let key: String = seg[..k]
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| s.as_str())
                .collect();
            if key.is_empty() {
                continue;
            }
            let mut pushed = 0usize;
            for e in &self.dict.exact(&key) {
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    k,
                ));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }
        }
        cands
    }

    /// 混拼档：多段混合（`nhao` → n 简拼 + hao 完整）→ 不完整段展开为音节列表，
    /// 逐级笛卡尔积 exact 查询（词频合并）；组合数超限该级降级为空。
    fn mixed_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
        const PER_LEVEL_EXACT: usize = 20;
        const MAX_EXPAND_QUERIES: usize = 2000;
        let n = seg.len();
        let mut cands = Vec::new();
        for k in (1..=n).rev() {
            // 展开前 k 段：完整段→自身；不完整段→音节前缀列表
            let mut lists: Vec<Vec<&str>> = Vec::new();
            let mut product: usize = 1;
            let mut ok = true;
            for s in &seg[..k] {
                if s.is_empty() {
                    ok = false;
                    break;
                }
                if self.is_syllable(s) {
                    lists.push(vec![s.as_str()]);
                } else {
                    let l: Vec<&str> = self
                        .dict
                        .syllables()
                        .iter()
                        .filter(|syl| syl.starts_with(s))
                        .map(|x| x.as_str())
                        .collect();
                    if l.is_empty() {
                        ok = false;
                        break;
                    }
                    product *= l.len();
                    if product > MAX_EXPAND_QUERIES {
                        ok = false;
                        break;
                    }
                    lists.push(l);
                }
            }
            if !ok {
                continue; // 该级降级为空
            }
            // 笛卡尔积 → exact 查询 → 词频合并
            let mut combos: Vec<Vec<&str>> = vec![Vec::new()];
            for l in &lists {
                let mut next = Vec::with_capacity(combos.len() * l.len());
                for c in &combos {
                    for syl in l {
                        let mut cc = c.clone();
                        cc.push(syl);
                        next.push(cc);
                    }
                }
                combos = next;
            }
            let mut entries: Vec<iuv_data::Entry> = Vec::new();
            for combo in &combos {
                entries.extend(self.dict.exact(&combo.join("'")));
            }
            entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
            let mut seen = std::collections::HashSet::new();
            let mut pushed = 0usize;
            for e in entries {
                if !seen.insert(e.word.clone()) {
                    continue;
                }
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    k,
                ));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }
        }
        cands
    }

    /// 现有全拼路径（含歧义单音节）：砍尾巴逐级匹配。
    /// for k = n..1，对前缀 `seg[0..k]` 跑 viterbi（每级 0 或 1 句）
    /// 加前缀枚举切分查 exact（词/单字）；候选按前缀长度从长到短排列，
    /// 同 k 内 Sentence 在前、词按权重降序。viterbi.rs 算法零改动。
    fn full_pinyin_candidates(
        &self,
        raw: &str,
        plain: &str,
        seg: &[String],
    ) -> Vec<crate::Candidate> {
        // 每级词候选上限（"2/3 字词时几个/十几个候选词"的规模；全局另有 max_candidates 截断）。
        const PER_LEVEL_EXACT: usize = 20;

        let mut cands: Vec<crate::Candidate> = Vec::new();
        let n = seg.len();

        for k in (1..=n).rev() {
            let prefix = &seg[..k];

            // 1. 每级整句——**遍历该前缀的所有切分方案**（消费端遍历所有方案，
            //    2026-08-14：keneng 的 [ke,neng]（可能）与 [ken,eng]（啃嗯）都组句，
            //    按 viterbi 分排序——词条直接命中分高者第一；不再只看贪心方案[0]）。
            //    raw 含撇号（强制输入 xi'an/fen'ge）保持仅 prefix 方案组句（撇号语义不破）。
            if k >= 2 {
                let consumed_chars: usize = prefix.iter().map(|s| s.len()).sum();
                let mut sentences: Vec<(crate::Candidate, f64)> = Vec::new();
                let plan_segs: Vec<Vec<String>> = if raw.contains('\'') {
                    vec![prefix.to_vec()]
                } else {
                    self.schema
                        .segment(&plain[..consumed_chars.min(plain.len())])
                };
                for plan in plan_segs {
                    let vseg: Vec<String> =
                        plan.iter().filter(|s| !s.is_empty()).cloned().collect();
                    // 只对**全完整方案**组句（2026-08-14 修复）：含兜底段的方案
                    // （[ke,nen,g] 的 g、[ke,ne,ng] 的 ng 非法音节）不组句——否则
                    // "可嫩g/啃嗯g/可呢ng/跌r" 等劣质整句进候选（且 commit 时
                    // seg_len > 当前段数导致数组越界 panic，实测 2026-08-14）。
                    // 兜底段是切分"保证有解"的产物，不是真实音节，组句无意义。
                    if vseg.len() >= 2 && vseg.iter().all(|s| self.is_syllable(s)) {
                        if let Some((sentence, score)) = crate::viterbi::best_sentence_scored(
                            &self.dict,
                            &vseg,
                            &*self.lm,
                            &self.config,
                        ) {
                            // M2 隐藏（Shift+Delete）：屏蔽组合对整句同样生效——词条级由
                            // Dict::merged 过滤，整句级在此拦截（用户隐藏的 (code, text)
                            // 组合不再被 viterbi 组出；否则隐藏"手癣"后整句「手癣」
                            // （手+癣单字）仍会出现，隐藏失效）。
                            let blocked = self
                                .dict
                                .user()
                                .map(|u| u.is_blocked(&sentence.code, &sentence.text))
                                .unwrap_or(false);
                            if !blocked {
                                sentences.push((sentence, score));
                            }
                        }
                    }
                }
                sentences
                    .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let mut seen = std::collections::HashSet::new();
                for (sentence, _) in sentences {
                    if seen.insert(sentence.text.clone()) {
                        cands.push(sentence);
                    }
                }
            }

            // 2. 前缀枚举切分 → exact 词/单字（join 键）。枚举源必须是无撇号的 plain 前缀：
            //    多段方案 join(') 后段内枚举会被 `'` 强制切分扼杀（fenge 只出 feng'e、
            //    keneng 只出 ken'eng，fen'ge/ke'neng 不可及）；raw 含撇号（强制输入如
            //    xi'an/fen'ge）保持 join 键不枚举，强制语义不破。
            let consumed_chars: usize = prefix.iter().map(|s| s.len()).sum();
            let mut keys = vec![prefix.join("'")];
            if !raw.contains('\'') {
                for plan in self
                    .schema
                    .segment(&plain[..consumed_chars.min(plain.len())])
                {
                    keys.push(plan.join("'"));
                }
            }
            let mut entries: Vec<iuv_data::Entry> = Vec::new();
            for key in keys {
                entries.extend(self.dict.exact(&key));
            }
            entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
            let mut seen = std::collections::HashSet::new();
            let mut pushed = 0usize;
            for e in entries {
                if !seen.insert(e.word.clone()) {
                    continue;
                }
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(e.word, kind, e.code, e.weight, k));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }

            // 3. 最后一级（k=1，第一段单字）**追加单字全量**（微软对齐：多段输入翻页
            //    可达低频同音字，如 zhangweiwei→选张→weiwei 续接翻页取「葳」；原
            //    PER_LEVEL_EXACT=20 把低频字卡在边界，实测 2026-08-14）。追加而非
            //    替换：歧义单音节（xian→[xi,an]）的枚举替代切分词（西安）必须保留，
            //    重复单字由 generate_candidates 末尾全局 text 去重兜底（保序先见先留）。
            //    单段档语义：完整音节 → exact_single 全量；严格前缀 → 首字母桶。
            if k == 1 {
                cands.extend(self.single_segment_candidates(&seg[0]));
            }
        }

        cands
    }

    /// 用户选择记录（commit 时调用）。
    pub(crate) fn record_selection(&self, code: &str, text: &str) {
        let mut store = self.store.lock().expect("store lock poisoned");
        store.record_selection(code, text, SystemTime::now());
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

    fn user_weight(e: &Engine, code: &str, word: &str) -> Option<u32> {
        e.dict.user().and_then(|u| {
            u.adjusted(code)
                .iter()
                .find(|(w, _)| w == word)
                .map(|(_, a)| *a)
        })
    }

    #[test]
    fn record_phrase_weight_scenarios() {
        // a：无命中 → 8000
        let e = Engine::new(
            dict_of(vec![
                ("zhang".into(), "张", 90000),
                ("wei".into(), "威", 1000),
                ("wei".into(), "葳", 50),
            ]),
            Config::default(),
        );
        e.record_phrase("zhang'wei'wei", "张葳葳");
        assert_eq!(
            user_weight(&e, "zhang'wei'wei", "张葳葳"),
            Some(8000),
            "场景 a 常量权重"
        );

        // b1：n=2 < page_size=5 → 手癣(300)−1 = 299
        let e = Engine::new(
            dict_of(vec![
                ("shou'xuan".into(), "首选", 8000),
                ("shou'xuan".into(), "手癣", 300),
            ]),
            Config::default(),
        );
        e.record_phrase("shou'xuan", "手选");
        assert_eq!(
            user_weight(&e, "shou'xuan", "手选"),
            Some(299),
            "b1：n−1 位减一"
        );

        // b2：n=6 >= page_size=5 → avg(中芯3000, 众心1000) = 2000
        let e = Engine::new(
            dict_of(vec![
                ("zhong'xin".into(), "中心", 9000),
                ("zhong'xin".into(), "衷心", 7000),
                ("zhong'xin".into(), "钟鑫", 5000),
                ("zhong'xin".into(), "中芯", 3000),
                ("zhong'xin".into(), "众心", 1000),
                ("zhong'xin".into(), "忠信", 800),
            ]),
            Config::default(),
        );
        e.record_phrase("zhong'xin", "中信");
        assert_eq!(
            user_weight(&e, "zhong'xin", "中信"),
            Some(2000),
            "b2：avg(第4, 第5位)"
        );

        // 场景 0：词库已有 → 跳过（不记录）
        let e = Engine::new(
            dict_of(vec![("zhang'wei'wei".into(), "张威威", 6000)]),
            Config::default(),
        );
        e.record_phrase("zhang'wei'wei", "张威威");
        assert!(
            user_weight(&e, "zhang'wei'wei", "张威威").is_none(),
            "场景 0 不记录"
        );

        // 幂等：重复自造（用户库已有）→ 场景 0 拦截，权重不漂移
        e.record_phrase("zhang'wei'wei", "张威威");
        assert!(user_weight(&e, "zhang'wei'wei", "张威威").is_none());
        let e2 = Engine::new(
            dict_of(vec![
                ("shou'xuan".into(), "首选", 8000),
                ("shou'xuan".into(), "手癣", 300),
            ]),
            Config::default(),
        );
        e2.record_phrase("shou'xuan", "手选");
        e2.record_phrase("shou'xuan", "手选"); // 第二次：exact 含用户库手选 → 跳过
        assert_eq!(
            user_weight(&e2, "shou'xuan", "手选"),
            Some(299),
            "重复自造权重不漂移"
        );
    }

    #[test]
    fn hide_entry_removes_override_then_blocks() {
        // 用户库有条目（自造词）→ 隐藏 = 删除条目
        let e = Engine::new(
            dict_of(vec![
                ("shou'xuan".into(), "首选", 8000),
                ("shou'xuan".into(), "手癣", 300),
            ]),
            Config::default(),
        );
        e.record_phrase("shou'xuan", "手选");
        assert!(user_weight(&e, "shou'xuan", "手选").is_some());
        e.hide_entry("shou'xuan", "手选");
        assert!(
            user_weight(&e, "shou'xuan", "手选").is_none(),
            "隐藏自造词 = 删除条目"
        );
        assert!(
            !e.dict.user().unwrap().is_blocked("shou'xuan", "手选"),
            "删除分支不写屏蔽"
        );
        // 无用户库条目（基础库词）→ 屏蔽
        e.hide_entry("shou'xuan", "手癣");
        assert!(
            e.dict.user().unwrap().is_blocked("shou'xuan", "手癣"),
            "基础库词 → 屏蔽"
        );
        // 屏蔽词条 + 整句拦截：exact 与 viterbi 都不再出现（集成测试已验证候选层）
        let hits = e.dict.exact("shou'xuan");
        assert!(!hits.iter().any(|x| x.word == "手癣"));
    }
}
