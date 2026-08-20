//! 五档候选生成路由（契约 §4.2 路由表 + M1.5 微软实测对齐，见
//! docs/research/msime-probe-checklist.txt）。`Engine::classify`（engine.rs）判定档位，
//! `generate_candidates`（engine.rs）按档位分派到本模块各函数。

use crate::engine::Engine;
use crate::Config;

/// 候选生成档位（契约 §4.2 路由表的一等概念）：输入经 `Engine::classify` 归入
/// 微软实测的 5 档 + 空档，`generate_candidates` 按档位分派，后续加档（M3 模糊音
/// 等）= 加一个 Route 臂，不再往分派函数里叠 if。
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
    /// 单段非前缀（`i`/`u`/`v`）或空输入 → 无候选（生成末尾有原文兜底，见 generate_candidates）
    Empty,
}

impl Engine {
    /// 单段档：完整音节或音节前缀 → 纯单字（词频序）。空档（i/u/v）由 classify 拦截。
    /// 微软实测：单段输入无论完整与否只出单字（shi→是时十使，无"时间/时候"）。
    /// 数据源分两路（M1.5 修正）：完整音节 → `dict.exact_single` 全部同音字（首字母桶
    /// 混收多字词会把同音字挤出 top-N，如 shi 只剩 5 字）；严格前缀 → 单字桶
    /// （桶只收单字，前缀无法 exact）。
    /// 候选**全量返回不截断**（微软对齐：sh 候选 600+ 全给、翻页可达，见
    /// docs/research/msime-probe-checklist.txt G3），由全局 max_candidates 兜底。
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
    /// 每级 k：exact(前 k 段首字母串)；尾巴段由 session 悬空续接重建为组合。
    /// 微软实测：简拼只出词（纯 exact 匹配，无单字、无更长词前缀）。
    /// **每级全量不截断**（2026-08-19）：简拼键是首字母严格匹配（`jj` 只出 j-j 词、
    /// 吉安在 `ja` 桶），无字母歧义可枚举——微软语义翻页可达低频词；全局 max_candidates 兜底。
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
    /// **每级全量不截断**（2026-08-19）：组合量已被 MAX_EXPAND_QUERIES 剪枝，语义同微软
    /// 「默认不设限」（15-input-matching.md §6）；全局 max_candidates 兜底。
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
    /// - **2b**：末段为音节前缀（`…chengy` 的 `y`）→ 把末段补全为所有合法音节
    ///   （`y` → yu/yi/yang/ying/…），**每个补齐方案各跑一次** Viterbi；
    /// - 全部句子按 viterbi 路径分取最高一条（M2 屏蔽组合拦截）。
    /// 前置守卫：除末段外其余段必须全为完整音节（Mixed 中段不完整如 `nhao` 不组句）。
    /// 2b 句子文本可能超出已敲字母（`shigechengy` → 「是一个成语」），预编辑显示仍按
    /// 输入切分不扩展（session 预览规则，见 session.rs candidate_preview 第 3 条）。
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
    /// **整句通道**（唯一）：`sentence_candidates` 一次 Viterbi（2a）/逐补齐一次（2b），
    /// 至多一条 Sentence，排最前。
    /// **词条通道**：k=n..1 砍末音节——合法砍完整音节（`…chengyu` 砍 `yu`）、不合法砍
    /// 不完整段（`…chengy` 砍 `y`，两路砍完第一刀后前缀对齐）——对剩余前缀枚举切分查
    /// exact（词/单字），命中才录；k=1 追加单字全量。排序 = 整句 → exact 匹配长度从长到短
    /// → 同 k 按权重降序。viterbi.rs 算法零改动。
    pub(crate) fn full_pinyin_candidates(
        &self,
        raw: &str,
        plain: &str,
        seg: &[String],
    ) -> Vec<crate::Candidate> {
        // 词条通道每级全量不截断（2026-08-19）：截断会饿死低权重替代切分词（jian 的 20
        // 位权重 492 把吉安 160/集安 31 挤出——西安 6091 靠运气挤进前 20）；单字全量由
        // k=1 追加兜底，词全量翻页可达即微软语义。全局 max_candidates 截断。
        // 配置快照（viterbi 需 &Config；热载 set_config 与读取并发安全）。
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let mut cands: Vec<crate::Candidate> = Vec::new();
        let n = seg.len();

        // 1. 整句通道：唯一一次 Viterbi（至多一条 Sentence），置候选最前。
        //    2a 结尾合法 → 整串一次；2b 结尾不合法 → 末段逐补齐一次，取分最高。
        //    不因切分方案、不因 k 消费长度产生多条整句（2026-08-18 重写）。
        let vseg: Vec<String> = seg.iter().filter(|s| !s.is_empty()).cloned().collect();
        if let Some(sentence) = self.sentence_candidates(&vseg, &cfg) {
            cands.push(sentence);
        }

        // 2. 词条通道：砍尾巴逐级 exact（k=n..1，命中才录）。raw 含撇号（强制输入
        //    xi'an/fen'ge）保持 join 键不枚举，强制语义不破。
        for k in (1..=n).rev() {
            let prefix = &seg[..k];

            // 前缀枚举切分 → exact 词/单字（join 键）。枚举源必须是无撇号的 plain 前缀：
            //    多段方案 join(') 后段内枚举会被 `'` 强制切分扼杀（fenge 只出 feng'e、
            //    keneng 只出 ken'eng，fen'ge/ke'neng 不可及）；raw 含撇号（强制输入如
            //    xi'an/fen'ge）保持 join 键不枚举，强制语义不破。
            // **k=1 例外（2026-08-18 续接修复）**：续接尾巴的撇号是引擎注入的
            //    （session::commit_index `seg[consumed..].join("'")`），非用户强制——首段
            //    歧义音节（xian→[xi,an]）被撇号锁死 → 「西安」不可达（实测：手选 德国
            //    老师 在 后尾巴 xian'chi'… 西安进不了候选）。k=1 即「对砍尾结果重新
            //    分音节逐 variant exact」（与独立输入 xian 的 AmbiguousSyllable 通道同
            //    路径），只枚举首段替代切分，不破坏 k≥2 强制语义。
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
            //    可达低频同音字，如 zhangweiwei→选张→weiwei 续接翻页取「葳」。追加而非
            //    替换：歧义单音节（xian→[xi,an]）的枚举替代切分词（西安）必须保留，
            //    重复单字由 generate_candidates 末尾全局 text 去重兜底（保序先见先留）。
            //    单段档语义：完整音节 → exact_single 全量；严格前缀 → 首字母桶。
            if k == 1 {
                cands.extend(self.single_segment_candidates(&seg[0]));
            }
        }

        cands
    }
}

/// 按 word 去重后把词条转候选推入（P1.6 抽取：混拼/全拼词条通道共用样板；
/// 保序先见先留，与 `generate_candidates` 末尾全局 text 去重语义一致）。
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