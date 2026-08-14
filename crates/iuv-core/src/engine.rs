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
        // 持久化 + mtime 基线刷新（防止本进程下次会话对自写文件无谓重载）
        let mut state = self.user_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = &state.path {
            if next.save(path).is_ok() {
                state.mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
            }
        }
        drop(state);
        self.dict.set_user(Arc::new(next));
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 调试/REPL 用精确查询。
    pub fn lookup(&self, squashed_code: &str) -> Vec<Entry> {
        self.dict.exact(squashed_code)
    }

    /// 档位判定（唯一判定点）：`plain` 为去 `'` 的整串，`plans_len` 为切分方案数。
    /// 与契约 §4.2 路由表逐行对应：
    /// - 整串是音节真前缀 → 单字档（切分器可能把它切成多段，但微软实测只出单字）
    /// - 单段完整音节：无替代切分 → 单字档；有替代切分（xian）→ 全拼（混排西安）
    /// - 单段非前缀（i/u/v）→ 空
    /// - 多段：按段完整性分派（全完整/全不完整/混合）
    fn classify(&self, plain: &str, seg: &[String], plans_len: usize) -> Route {
        if !plain.is_empty() && self.is_syllable_prefix(plain) && !self.is_syllable(plain) {
            return Route::PrefixChars;
        }
        if seg.len() == 1 {
            if self.is_syllable(&seg[0]) {
                return if plans_len > 1 {
                    Route::AmbiguousSyllable
                } else {
                    Route::CompleteChars
                };
            }
            return Route::Empty;
        }
        let kinds: Vec<bool> = seg
            .iter()
            .map(|s| !s.is_empty() && self.is_syllable(s))
            .collect();
        if kinds.iter().all(|&c| c) {
            Route::FullPinyin
        } else if kinds.iter().all(|&c| !c) {
            Route::Abbrev
        } else {
            Route::Mixed
        }
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
        plans_len: usize,
    ) -> Vec<crate::Candidate> {
        let plain: String = raw.chars().filter(|c| *c != '\'').collect();
        let mut cands = match self.classify(&plain, seg, plans_len) {
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

    fn is_syllable(&self, s: &str) -> bool {
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

            // 1. 每级一句整句（k >= 2）。空段（尾/连续 `'`）过滤后组句，防兜底空词。
            if k >= 2 {
                let vseg: Vec<String> = prefix.iter().filter(|s| !s.is_empty()).cloned().collect();
                if vseg.len() >= 2 {
                    if let Some(sentence) =
                        crate::viterbi::best_sentence(&self.dict, &vseg, &*self.lm, &self.config)
                    {
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
        }

        cands
    }

    /// 用户选择记录（commit 时调用）。
    pub(crate) fn record_selection(&self, code: &str, text: &str) {
        let mut store = self.store.lock().expect("store lock poisoned");
        store.record_selection(code, text, SystemTime::now());
    }
}
