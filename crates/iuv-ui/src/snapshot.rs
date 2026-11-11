//! 候选窗 UI 快照类型（自 iuv-tsf/src/ui/mod.rs 迁入，字段/语义保留，
//! iuv-tsf 侧将改为 re-export 本模块类型）。

use iuv_core::{Effect, Orientation, PageInfo};

/// 光标矩形（屏幕坐标）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaretRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 候选窗 UI 快照。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiSnapshot {
    /// "ni'hao"
    pub reading: String,
    /// 页内候选文本
    pub candidates: Vec<String>,
    /// 全量候选文本（所有页，按页内序）。TSF 候选 UI 元素数据源（游戏内候选栏
    /// 翻页切片用）；自绘窗只消费当前页 candidates。
    pub all_candidates: Vec<String>,
    pub selected: usize,
    pub page: PageInfo,
    /// 布局方向（竖排/横排）；effect_to_snapshot 默认竖排，TSF 侧从 config 填
    pub orientation: Orientation,
    /// 62 号：当前候选组标签（如「首字 2/4」）；空 = 无分组信息，布局与旧版一致。
    pub group_label: String,
}

/// 是否需要底部小字行：多页，或存在分组标签。
pub fn show_page(snap: &UiSnapshot) -> bool {
    snap.page.page_count > 1 || !snap.group_label.is_empty()
}

/// 底部小字行的唯一文本口径（布局量测与渲染共用，避免漂移）。
pub fn page_label(snap: &UiSnapshot) -> String {
    let p = format!("{}/{}", snap.page.page + 1, snap.page.page_count.max(1));
    if snap.group_label.is_empty() {
        p
    } else {
        format!("{} {}", snap.group_label, p)
    }
}

/// Effect → UiSnapshot：取 effect.reading / 页内候选 text / selected / page。
pub fn effect_to_snapshot(e: &Effect) -> UiSnapshot {
    UiSnapshot {
        reading: e.reading.clone(),
        candidates: e.candidates.iter().map(|c| c.text.clone()).collect(),
        all_candidates: e.all_candidates.iter().map(|c| c.text.clone()).collect(),
        selected: e.selected,
        page: e.page,
        orientation: Orientation::default(),
        group_label: if e.groups.len() > 1 {
            let idx = (e.active_group as usize).min(e.groups.len() - 1);
            format!("{} {}/{}", e.groups[idx].label, idx + 1, e.groups.len())
        } else {
            String::new()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::{Candidate, CandidateKind};

    #[test]
    fn effect_to_snapshot_maps_fields() {
        let cands = vec![
            Candidate::new("你好", CandidateKind::Word, "nihao", 1, 2),
            Candidate::new("泥嚎", CandidateKind::Word, "nihao", 2, 2),
        ];
        let e = Effect {
            reading: "ni'hao".into(),
            candidates: cands.clone(),
            all_candidates: cands,
            selected: 1,
            page: PageInfo {
                page: 0,
                page_count: 2,
                page_size: 5,
                total: 7,
            },
            ..Default::default()
        };
        let snap = effect_to_snapshot(&e);
        assert_eq!(snap.reading, "ni'hao");
        assert_eq!(snap.candidates, vec!["你好", "泥嚎"]);
        assert_eq!(snap.all_candidates, vec!["你好", "泥嚎"]);
        assert_eq!(snap.selected, 1);
        assert_eq!(snap.page.page_count, 2);
    }
}
