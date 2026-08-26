//! classic 引擎核心（39-rime-pipeline.md §4「先拆后缝」）：候选生成逻辑本体。
//!
//! 代码自 engine.rs / routes.rs 平移（2026-08-26，Step 1 拆分）：本文件 = 分节方案
//! 重排 + 档位路由 + 五个生成器 + 统一收尾（联想/去重/截断/兜底），外加
//! [`ImeEngine`] 的 classic 实现。engine.rs 只留资源持有与管理。
//! **行为零变化红线**：平移不改逻辑，既有回归测试即验收线。

use crate::api::{EngineCtx, ImeEngine, PendingInput, Span, Translation};
use crate::engine::Engine;
use crate::Config;

/// 候选生成档位（契约 §4.2 路由表的一等概念）：输入经 [`classify`](Engine::classify)
/// 归入微软实测的 5 档 + 空档，[`generate_candidates`](Engine::generate_candidates)
/// 按档位分派。rime 核心落地后此枚举降级为 classic 内部实现细节。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// 严格前缀（`c`/`sh`/`zho`）→ 纯单字（单字桶）
    PrefixChars,
    /// 完整单音节无歧义（`shi`/`de`/`ba`）→ 纯单字（exact 全量）
    CompleteChars,
    /// 完整单音节有歧义（`xian`→[xi,an]）→ 全拼整句/词条两通道（替代切分词混排）
    AmbiguousSyllable,
    /// 多段全完整（`nihao`/`xi'an`）**或末音节可补全**（`shigechengy`→`y` 补 `yu/yi/…`）
    /// → 全拼两通道：整句通道唯一一次 Viterbi（2a 直接 / 2b 补全） + 词条通道砍尾巴 exact
    FullPinyin,
    /// 多段全不完整（`nh`/`nhm`）→ 简拼键逐级砍尾巴
    Abbrev,
    /// 多段混合且**中段不完整**（`nhao`）→ 简拼段展开配对（无整句通道）
    Mixed,
    /// 单段非前缀（`i`/`u`/`v`）或空输入 → 无候选（生成末尾有原文兜底）
    Empty,
}

impl Engine {
    /// 单段档：完整音节或音节前缀 → 纯单字（词频序）。空档（i/u/v）由 classify 拦截。
    /// 微软实测：单段输入无论完整与否只出单字（shi→是时十使，无"时间/时候"）。
    /// 数据源分两路（M1.5 修正）：完整音节 → `dict.exact_single` 全部同音字（首字母桶
    /// 混收多字词会把同音字挤出 top-N，如 shi 只剩 5 字）；严格前缀 → 单字桶
    /// （桶只收单字，前缀无法 exact）。
    /// 候选**全量返回不截断**（微软对齐：sh 候选 600+ 全给、翻页可达），由全局
    /// max_candidates 兜底。
    pub(crate) fn single_segment_candidates(&self, s: &str) -> Vec<crate::Candidate> {
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
            .map(|e| crate::Candidate::for_entry(&e, crate::CandidateKind::Char, 1))
            .collect()
    }

    /// 简拼键档：多段全不完整（`nh`/`nhm`/`nhmsx`）→ 构建期简拼键逐级砍尾巴。
    /// 微软实测：简拼只出词（纯 exact 匹配，无单字、无更长词前缀）。
    /// **每级全量不截断**（2026-08-19）：简拼键是首字母严格匹配，全局 max_candidates 兜底。
    pub(crate) fn abbrev_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
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
            for e in &self.dict.exact(&key) {
                let kind = crate::CandidateKind::for_word(&e.word);
                cands.push(crate::Candidate::for_entry(e, kind, k));
            }
        }
        cands
    }

    /// 混拼档：多段混合（`nhao` → n 简拼 + hao 完整）→ 不完整段展开为音节列表，
    /// 逐级笛卡尔积 exact 查询（词频合并）；组合数超限该级降级为空。
    /// **每级全量不截断**（2026-08-19）：组合量已被 MAX_EXPAND_QUERIES 剪枝；
    /// 全局 max_candidates 兜底。
    pub(crate) fn mixed_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
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
            for e in entries {
                push_unique_entry(&mut cands, &mut seen, &e, k);
            }
        }
        cands
    }

    /// 整句通道（2026-08-18，词库负责"词"、Viterbi 只负责"唯一最佳句子"）。
    /// 对未选中部分至多产出**一条** Sentence：
    /// - **2a**：末段为完整音节（`…chengyu`）→ 整串跑一次 Viterbi；
    /// - **2b**：末段为音节前缀（`…chengy` 的 `y`）→ 把末段补全为所有合法音节，
    ///   每个补齐方案各跑一次；全部句子按路径分取最高（M2 屏蔽拦截）。
    /// 前置守卫：除末段外其余段必须全为完整音节。
    fn sentence_candidates(&self, vseg: &[String], cfg: &Config) -> Option<crate::Candidate> {
        if vseg.len() < 2 {
            return None;
        }
        let (last, rest) = vseg.split_last()?;
        if !rest.iter().all(|s| self.is_syllable(s)) {
            return None;
        }
        let sentences: Vec<(crate::Candidate, f64)> = if self.is_syllable(last) {
            // 2a：结尾完整音节 → 唯一一次 Viterbi
            crate::viterbi::best_sentence_scored(&self.dict, vseg, &*self.lm, cfg)
                .into_iter()
                .collect()
        } else {
            // 2b：末段补全为所有合法音节，逐个补齐跑 Viterbi
            self.dict
                .syllables()
                .iter()
                .filter(|s| s.starts_with(last.as_str()))
                .filter_map(|comp| {
                    let mut s = rest.to_vec();
                    s.push(comp.clone());
                    crate::viterbi::best_sentence_scored(&self.dict, &s, &*self.lm, cfg)
                })
                .collect()
        };
        let best = sentences
            .into_iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;
        let blocked = self
            .dict
            .user()
            .map(|u| u.is_blocked(&best.0.code, &best.0.text))
            .unwrap_or(false);
        if blocked {
            None
        } else {
            Some(best.0)
        }
    }

    /// 现有全拼路径（含歧义单音节与末音节可补全 2b 场景）：两通道。
    /// **整句通道**（唯一）：至多一条 Sentence，置候选最前。
    /// **词条通道**：k=n..1 砍末音节——合法砍完整音节、不合法砍不完整段——对剩余
    /// 前缀枚举切分查 exact（词/单字），命中才录；k=1 追加单字全量。
    /// 排序 = 整句 → exact 匹配长度从长到短（k 降序）→ 同 k 按权重降序。
    pub(crate) fn full_pinyin_candidates(
        &self,
        raw: &str,
        plain: &str,
        seg: &[String],
    ) -> Vec<crate::Candidate> {
        // 词条通道每级全量不截断（2026-08-19）：截断会饿死低权重替代切分词；
        // 单字全量由 k=1 追加兜底。全局 max_candidates 截断。
        // 配置快照（viterbi 需 &Config；热载 set_config 与读取并发安全）。
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let mut cands: Vec<crate::Candidate> = Vec::new();
        let n = seg.len();

        // 1. 整句通道：唯一一次 Viterbi（至多一条 Sentence），置候选最前。
        let vseg: Vec<String> = seg.iter().filter(|s| !s.is_empty()).cloned().collect();
        if let Some(sentence) = self.sentence_candidates(&vseg, &cfg) {
            cands.push(sentence);
        }

        // 2. 词条通道：砍尾巴逐级 exact（k=n..1，命中才录）。raw 含撇号（强制输入
        //    xi'an/fen'ge）保持 join 键不枚举，强制语义不破。
        for k in (1..=n).rev() {
            let prefix = &seg[..k];

            // 前缀枚举切分 → exact 词/单字（join 键）。枚举源必须是无撇号的 plain 前缀。
            // **k=1 例外（2026-08-18 续接修复）**：续接尾巴的撇号是引擎注入的，非用户
            // 强制——首段歧义音节被撇号锁死 → 「西安」不可达。k=1 即对砍尾结果重新
            // 分音节逐 variant exact，只枚举首段替代切分，不破坏 k≥2 强制语义。
            let consumed_chars: usize = prefix.iter().map(|s| s.len()).sum();
            let mut keys = vec![prefix.join("'")];
            if !raw.contains('\'') || k == 1 {
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
            for e in entries {
                push_unique_entry(&mut cands, &mut seen, &e, k);
            }

            // 3. 最后一级（k=1，第一段单字）**追加单字全量**（微软对齐：多段输入翻页
            //    可达低频同音字）。追加而非替换，重复由全局 text 去重兜底。
            if k == 1 {
                cands.extend(self.single_segment_candidates(&seg[0]));
            }
        }

        cands
    }

    /// 档位判定（唯一判定点）：`plain` 为去 `'` 的整串，`plans` 为全部切分方案
    /// （消费端遍历所有方案）。与契约 §4.2 路由表逐行对应：
    /// - 整串是音节真前缀 → 单字档（切分器可能把它切成多段，但微软实测只出单字）
    /// - 单段完整音节：无替代切分 → 单字档；有替代切分（xian）→ 全拼（混排西安）
    /// - 单段非前缀（i/u/v）→ 空
    /// - 多段：**存在任一全完整方案 → 全拼**；**末音节可补全 → 也归全拼 2b**；
    ///   否则按段完整性分派（全不完整 → 简拼；混合 → 混拼）
    pub(crate) fn classify(&self, plain: &str, seg: &[String], plans: &[Vec<String>]) -> Route {
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
        // → 归入全拼档走 2b 整句通道。
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
    /// 跟随用户最可能打的词。排序键 = 方案 join 键 exact 词条最大权重（词条优先；
    /// 无词条 = 0），稳定排序保贪心原序。（原 session 显式调用，Step 1 收编进核心。）
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

    /// 按契约 §4.2 生成候选（编排本体，原 engine.rs 函数整体平移）。
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

        // 兜底：所有路由均无候选且输入非空 → 原文候选（"不认识"语义）。
        // 无编号呈现由 UI 按 text == 预编辑原文 判定（Step 3 改显式类型标记）。
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
}

/// [`ImeEngine`] 的 classic 实现：编排收编（segment → rank_plans → generate 一气呵成，
/// 会话层不再分步调用），行为与拆分前完全一致。
impl ImeEngine for Engine {
    fn translate(&self, _ctx: &EngineCtx, pending: &PendingInput) -> Translation {
        let plans = self.schema.segment(pending.raw);
        let plans = self.rank_plans(plans);
        let seg = plans.first().cloned().unwrap_or_default();
        let candidates = self.generate_candidates(pending.raw, &seg, &plans);
        Translation {
            segmentation: vec![Span {
                syllables: seg,
                tags: vec!["pinyin"],
            }],
            candidates,
        }
    }

    /// 预编辑显示：五规则共享实现（api::preview_rules），seg = 方案重排后首段。
    fn preedit(
        &self,
        _ctx: &EngineCtx,
        pending: &PendingInput,
        selected: Option<&crate::Candidate>,
    ) -> String {
        let plans = self.schema.segment(pending.raw);
        let plans = self.rank_plans(plans);
        let seg = plans.first().cloned().unwrap_or_default();
        crate::api::preview_rules(
            pending.raw,
            &seg,
            &|s| self.is_syllable(s),
            &|s| self.schema.display(s),
            selected,
        )
    }
}

/// 按 word 去重后把词条转候选推入（保序先见先留，与 generate_candidates 末尾全局
/// text 去重语义一致）。
fn push_unique_entry(
    cands: &mut Vec<crate::Candidate>,
    seen: &mut std::collections::HashSet<String>,
    e: &iuv_data::Entry,
    k: usize,
) {
    if !seen.insert(e.word.clone()) {
        return;
    }
    cands.push(crate::Candidate::for_entry(
        e,
        crate::CandidateKind::for_word(&e.word),
        k,
    ));
}
