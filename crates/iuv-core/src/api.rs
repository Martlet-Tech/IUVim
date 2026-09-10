//! 顶层引擎接口（39-rime-pipeline.md §4）：唯一签名面。
//!
//! 本质两份输入输出：
//! ① 待输入串 → 分段视图 + 候选列表（[`ImeEngine::translate`]）；
//! ② 高亮候选 → 预编辑显示串（[`ImeEngine::preedit`]，如输入 `jian` 导航到
//!    「吉安」时返回 `ji'an`）。
//!
//! rime 核心实现此 trait；会话层只认它，不感知核心实现差异。
//! `EngineCtx::preceding_text` 为组句预埋钩子：rime 核心喂给组句打分
//! （poet 的 preceding_text 机制）。

/// 一次 translate/preedit 的上下文。
pub struct EngineCtx<'a> {
    /// 已确认前文（悬空选词拼接的汉字）。rime 组句上下文用。
    pub preceding_text: &'a str,
}

/// 待输入串：用户敲的原始字母串（可能含用户强制撇号 `'`）。
pub struct PendingInput<'a> {
    pub raw: &'a str,
}

/// 分段视图的一段：音节序列。
///
/// `syllables` 保留空段（尾撇号 display 语义，与既有 seg 一致）。
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub syllables: Vec<String>,
}

/// translate 输出：分段视图 + 活动段候选列表。
///
/// 打字期 rime 按「单活动段」覆盖重译（39-rime-pipeline.md 架构裁决），
/// `segmentation` 恒为单段（首段 = 方案词频重排后的贪心切分）。
#[derive(Clone, Debug, PartialEq)]
pub struct Translation {
    pub segmentation: Vec<Span>,
    pub candidates: Vec<Candidate>,
}

/// 顶层引擎接口。实现须线程安全（进程级单例跨线程共享）。
pub trait ImeEngine: Send + Sync {
    /// 输入方向①：待输入串 → 分段视图 + 候选列表。
    fn translate(&self, ctx: &EngineCtx, pending: &PendingInput) -> Translation;

    /// 输入方向②：当前高亮候选 → 该候选视角下的预编辑显示串（只含未消费尾巴，
    /// 已确认前文由会话层拼接）。`selected = None` 时返回默认切分显示。
    ///
    /// 46 号 §3.3 职责重划：**seg 由调用方提供**（= `session.self.seg`，
    /// 即 translate 产出的分段视图首段），本调用只做纯显示。旧签名内部自行重算
    /// 切分，实测这是每键与 translate 同量级的第二遍开销（长串哨兵累计约 5.9s）。
    /// 实现方在 `seg` 为空时可自行兜底（回落贪心），保证空分段不导致显示丢失。
    fn preedit(&self, raw: &str, seg: &[String], selected: Option<&Candidate>) -> String;
}

use crate::{Candidate, CandidateKind};

/// 预编辑显示五规则（rime 核心使用；判定顺序即契约顺序）：
/// 1. 用户强制撇号（raw 含 `'`）：恒输入切分，不跟随候选；
/// 2. 原文兜底（候选 text == 输入去撇号）：原样 plain 不分节；
/// 3. 消费段不完整（简拼 jisb/nh、前缀档）：输入切分；
/// 4. 消费段完整 且 候选 code（去撇号）== 输入：跟随候选切分（jian+吉安 → ji'an）；
/// 5. 其余：输入切分。
pub(crate) fn preview_rules(
    raw: &str,
    seg: &[String],
    is_syllable: &dyn Fn(&str) -> bool,
    display: &dyn Fn(&[String]) -> String,
    selected: Option<&Candidate>,
) -> String {
    let Some(c) = selected else {
        return display(seg);
    };
    if raw.contains('\'') {
        return display(seg);
    }
    let plain = crate::strip_apostrophes(raw);
    if c.text == plain {
        return plain;
    }
    let consumed = c.seg_len.max(1).min(seg.len());
    let consumed_full = seg[..consumed].iter().all(|s| !s.is_empty() && is_syllable(s));
    if !consumed_full {
        return display(seg);
    }
    let code_plain = crate::strip_apostrophes(&c.code);
    if code_plain == plain {
        let mut s = c.code.clone();
        if consumed < seg.len() {
            s.push('\'');
            s.push_str(&display(&seg[consumed..]));
        }
        return s;
    }
    display(seg)
}

/// 单字桶查询的共享实现（rime `prefix_chars_translation` 使用，2026-08-26 去重）：
/// 完整音节 → exact_single 全量；严格前缀 → 首字母桶过滤 starts_with。
pub(crate) fn single_char_entries(dict: &iuv_data::Dict, s: &str) -> Vec<iuv_data::Entry> {
    if s.is_empty() {
        return Vec::new();
    }
    if dict.is_syllable(s) {
        dict.exact_single(s)
    } else {
        let first = s.chars().next().unwrap();
        dict.initial_top(first, iuv_data::INITIAL_BUCKET_SIZE)
            .into_iter()
            .filter(|e| e.code.starts_with(s))
            .collect()
    }
}

/// 原文兜底候选（"不认识"语义，rime fallback 使用）：
/// 多字符 → Word，单字符 → Char；text == code == plain。
pub(crate) fn raw_fallback_candidate(plain: &str, seg_len: usize) -> Candidate {
    let kind = if plain.chars().count() >= 2 {
        CandidateKind::Word
    } else {
        CandidateKind::Char
    };
    Candidate::new(plain, kind, plain, 0, seg_len)
}

/// 切分决策唯一入口（46 号 §3.1）：**词库整跨词反查优先，否则贪心**。
///
/// 语义等价性论证（对照删除前的 `rank_plans`）：旧实现给每个枚举方案打
/// `exact(方案 join 键).first().weight`，得 0 分者在稳定排序下保持枚举原序
/// （枚举首方案 = 贪心），非零分 ⇔ 方案 join 键恰是词库某码。故最优方案 =
/// 「码去撇号 == raw 归一形、且分隔掩码覆盖用户撇号」的码中有效权重最高者，
/// 否则贪心——一次 O(log n + 码长) 反查，无需枚举。
///
/// 三处必须与旧语义逐字对齐的细节（46 号任务书 §3.1 未写明，实现补齐）：
/// 1. **贪心方案也参战**：反查段构建期已滤掉「等于运行时贪心码形」的键（死数据），
///    若只比较变体，`先`(xian, 9000) vs `西安`(xi'an, 800) 这类输入会从 `xian`
///    漂移成 `xi'an`。故贪心码权重一并比较，**严格大于**才切换（平局保贪心，
///    与旧稳定排序同义）。
/// 2. **归一口径**：[`crate::schema::normalize_input`]（仅 lue→lve / nue→nve，
///    长度不变、**不折叠大小写**——大写保形靠它，否则 `niHAO` 会变 `ni'hao`）。
/// 3. **撇号与空段**：`seps` = 用户强制撇号在去撇号 concat 坐标系的字节偏移，
///    只有分隔掩码覆盖它的码才合法；raw 含空段（尾/连续 `'`，仅服务 display）
///    时 concat 语义失真 → 直接贪心。
///
/// 有效权重统一走 `exact().first()`（merged 视图：叠加屏蔽/调权，与旧实现同源）。
/// 反查段缺失（旧词库）时反查恒空 → 自然退化贪心，不 panic、不阻塞。
pub(crate) fn best_seg(
    dict: &iuv_data::Dict,
    schema: &dyn crate::schema::InputSchema,
    raw: &str,
) -> Vec<String> {
    let greedy = schema.segment(raw);
    let normalized = crate::schema::normalize_input(raw);
    if normalized.is_empty() {
        return greedy;
    }
    // 去撇号 + 记录强制撇号偏移；空段（尾/连续 `'`）→ 反查无意义，直接贪心。
    let mut concat = String::with_capacity(normalized.len());
    let mut seps: Vec<usize> = Vec::new();
    for (i, part) in normalized.split('\'').enumerate() {
        if part.is_empty() {
            return greedy;
        }
        if i > 0 {
            seps.push(concat.len());
        }
        concat.push_str(part);
    }

    let greedy_weight = code_weight(dict, &greedy.join("'"));
    let mut best: Option<(u32, Vec<String>)> = None;
    for code in dict.reverse_candidates(&concat, &seps) {
        let w = code_weight(dict, &code);
        // 0 权重变体等价于「词库里没有这个词」（被屏蔽/无词条）——不参战。
        if w == 0 {
            continue;
        }
        // 同权重保构建序（构建期已固化 weight 降序 + 码序升序）。
        if best.as_ref().is_some_and(|(bw, _)| w <= *bw) {
            continue;
        }
        best = Some((w, code.split('\'').map(str::to_string).collect()));
    }
    match best {
        Some((w, seg)) if w > greedy_weight => seg,
        _ => greedy,
    }
}

/// 码的有效权重（merged 视图的最高词条权重；无词条 = 0）。
fn code_weight(dict: &iuv_data::Dict, code: &str) -> u32 {
    dict.exact(code).first().map(|e| e.weight).unwrap_or(0)
}
