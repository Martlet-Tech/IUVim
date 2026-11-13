//! 分组候选空间（62 号 · 结构化候选）。
//!
//! 旧模型把音节图里**所有切分路径**的候选按消费终点 `end` 降序灌进一条扁平表，
//! 再由 `max_candidates`（默认 1024）先到先得截断。结果是词库规模直接决定用户
//! 要翻多少页：`yinguai` 里 `ying`（借走 g 的死路切分）272 个同音字把 `yin`
//! 的单字挤到第 280 位，其中还有大量权重 0 的扩展区生僻字排在权重 27218 的
//! 「因」前面。
//!
//! 新模型：**候选按「用户下一步想干什么」分组**，组间不竞争名额。
//! - `Whole`：覆盖整串的词（exact）与句候选——「选中就上屏」；
//! - `Completion`：尾前缀补全词（如 `yin'gu` → 因故/银箍）；
//! - `Prefix`：只消费前 k 段的词/单字——「先上屏前半截，尾巴续接」；
//! - `Alternate`：不属于唯一权威切分的其它读法（ying/yi …）——默认折叠，可展开。
//!
//! 63 号：组内不限量（62 号的 `GROUP_LIMIT` 配额与 `Rare` 生僻字折叠组已删除），
//! 0 权重生僻字按权重正常排在各组尾部、翻页可达；`max_candidates` 只作
//! 全局安全阀。整串只有句候选而没有 exact 词时，默认组落在首个 `Prefix`/`Syllable`
//! 组，让词库未收录（OOV）的组合可以直接逐字构造。

use crate::{Candidate, CandidateKind};

/// 前缀补全组上限（长词联想，不占整串组名额）。
pub const COMPLETION_LIMIT: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind {
    Whole,
    Completion,
    Prefix,
    Alternate,
}
/// 组元信息：`len` 为该组在 `Translation.candidates` 展平表里的候选数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMeta {
    pub kind: GroupKind,
    pub label: String,
    pub len: usize,
    pub folded: bool,
}

/// 给 UI / 协议用的组摘要（不带候选本体）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub label: String,
    pub count: u32,
    pub folded: bool,
}

impl GroupMeta {
    pub fn info(&self) -> GroupInfo {
        GroupInfo {
            label: self.label.clone(),
            count: self.len as u32,
            folded: self.folded,
        }
    }
}

/// 按组元信息从展平表里切出第 `index` 组。
pub fn group_slice<'a>(
    cands: &'a [Candidate],
    groups: &[GroupMeta],
    index: usize,
) -> &'a [Candidate] {
    let mut start = 0usize;
    for (i, g) in groups.iter().enumerate() {
        let end = (start + g.len).min(cands.len());
        if i == index {
            return &cands[start..end];
        }
        start = end;
    }
    &[]
}

/// 展平表 → 组摘要列表。
pub fn infos(groups: &[GroupMeta]) -> Vec<GroupInfo> {
    groups.iter().map(GroupMeta::info).collect()
}

/// 所有非折叠组的候选展平（兼容旧消费方 / 诊断用）。
pub fn main_flat(cands: &[Candidate], groups: &[GroupMeta]) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for g in groups {
        let end = (start + g.len).min(cands.len());
        if !g.folded {
            out.extend_from_slice(&cands[start..end]);
        }
        start = end;
    }
    out
}

/// 把「按 end 降序分桶」的候选整理成分组空间。
///
/// - `by_end`：`(end, 桶内候选)`，已按 end 降序；
/// - `bounds`：唯一权威切分的累计字节边界（含 0 与串尾）；
/// - `total_len`：图覆盖的总字节数；
/// - `sentence`：句候选（0/1 条），置顶于 `Whole` 组。
///
/// 返回 `(展平候选, 组元信息, 默认组索引)`。
pub fn build(
    mut by_end: Vec<(usize, Vec<Candidate>)>,
    bounds: &[usize],
    total_len: usize,
    sentence: Vec<Candidate>,
    cap: usize,
) -> (Vec<Candidate>, Vec<GroupMeta>, usize) {
    let mut whole = sentence;
    let mut completion: Vec<Candidate> = Vec::new();
    let mut prefixes: Vec<(usize, Vec<Candidate>)> = Vec::new();
    let mut alternate: Vec<Candidate> = Vec::new();

    for (end, bucket) in by_end.drain(..) {
        if end == total_len {
            whole.extend(bucket);
        } else if bounds.contains(&end) {
            prefixes.push((end, bucket));
        } else if end > 0 && end < total_len {
            // 尾前缀补全（end 接近串尾但不落在段边界）与死路切分分开：
            // 后者通常伴随大量单字（ying/yi），前者多为少量词。
            let chars = bucket
                .iter()
                .filter(|c| c.kind == CandidateKind::Char)
                .count();
            if chars == 0 && completion.len() < COMPLETION_LIMIT {
                completion.extend(bucket);
            } else {
                alternate.extend(bucket);
            }
        }
    }

    // 组顺序 = 用户意图优先级：整串 → 补全词 → 逐级前缀（end 降序 = 长前缀先） →
    // 折叠的备选切分。
    // 用宏而非闭包：闭包会长期持有 groups/flat 的可变借用，期间读 groups.len() 冲突。
    let mut groups: Vec<GroupMeta> = Vec::new();
    let mut flat: Vec<Candidate> = Vec::new();
    macro_rules! push_group {
        ($kind:expr, $label:expr, $items:expr, $folded:expr) => {{
            let mut items = $items;
            let folded = $folded;
            // 主空间累计不超过 `max_candidates`（组边界与候选表严格一致，
            // 截断后 GroupMeta.len 与实际展平条目数仍然对齐）；折叠组各自封顶。
            if folded {
                items.truncate(cap);
            } else {
                items.truncate(cap.saturating_sub(flat.len()));
            }
            let len = items.len();
            if len != 0 {
                groups.push(GroupMeta {
                    kind: $kind,
                    label: $label,
                    len,
                    folded: $folded,
                });
                flat.extend(items);
            }
        }};
    }

    // 组顺序 = 用户意图优先级：整串 → 补全词 → 逐级前缀（end 降序 = 长前缀先）。
    // 主空间是这些组的**展平表**（会话仍按一条表翻页，逐级候选保持可达），
    // 组边界只用于配额裁剪 / 组标签 / `]` `[` 跳组。
    let mut pending: Vec<(GroupKind, String, Vec<Candidate>, bool)> = Vec::new();
    let whole_only_sentence = whole.len() == 1 && whole[0].kind == CandidateKind::Sentence;
    pending.push((GroupKind::Whole, "整串".into(), whole, false));
    pending.push((GroupKind::Completion, "补全词".into(), completion, false));
    for (end, items) in prefixes.into_iter() {
        if items.is_empty() {
            continue;
        }
        let k = bounds.iter().position(|&b| b == end).unwrap_or(1);
        let label = if k <= 1 {
            "首字".into()
        } else {
            format!("前 {} 段", k)
        };
        pending.push((GroupKind::Prefix, label, items, false));
    }
    // OOV 直达：整串只有句候选（词库无此词）**且逐级前缀只有单字**时，把首字组
    // 提到最前——`yinguai` 第一页就是「因 音 银 印 引」，逐字构造「引怪」无需翻页。
    // 前缀组里若有词（如 `xihuan'chishuiguo` 的「喜欢」），说明组句通道仍有用，
    // 保持整句候选首选，避免破坏逐词续接的收尾流程。
    // 稳定排序保持各组内部与 Prefix 之间的原有次序。
    let prefix_all_chars = pending
        .iter()
        .filter(|(k, _, _, _)| *k == GroupKind::Prefix)
        .all(|(_, _, items, _)| items.iter().all(|c| c.kind == CandidateKind::Char));
    if whole_only_sentence && prefix_all_chars {
        pending.sort_by_key(|(k, _, _, _)| if *k == GroupKind::Prefix { 0 } else { 1 });
    }
    for (kind, label, items, folded) in pending {
        push_group!(kind, label, items, folded);
    }
    // 折叠组不进主空间分页，仍受全局安全阀约束（避免长串下展平表无限膨胀）。
    alternate.truncate(cap);
    push_group!(GroupKind::Alternate, "其它切分".into(), alternate, true);

    // 会话默认展示主空间展平表（所有非折叠组），故默认活动组恒为 0；
    // `active_group` 由当前页位置推算，用于窗口组标签。
    (flat, groups, 0)
}
