//! 候选窗纯布局函数（自 iuv-tsf/src/ui/gdi.rs 迁入，断言零改动随迁）。
//!
//! layout / hit_test / 定位三件套全部为纯函数，不依赖平台：
//! - `layout`：竖排每候选一行（页码右对齐末行）/ 横排单行（页码行尾右侧）；
//!   原文兜底候选（text == 预编辑原文去 `'`）不编号；
//! - `hit_test`：坐标落在哪个候选矩形上（横竖统一）；
//! - `position_for` / `position_in_area` / `update_position`：caret 定位
//!   下方优先、工作区内收、超屏翻到 caret 上方。
//!
//! 工作区矩形用 `Area`（left/top/right/bottom，与原 windows `RECT` 同构）
//! 替代——iuv-ui 跨平台纯 Rust，不得依赖 windows crate。

use crate::snapshot::{CaretRect, UiSnapshot};
use iuv_core::Orientation;

// ===== 布局常量 =====
pub const PAD_X: i32 = 8;
pub const PAD_Y: i32 = 4;
pub const ROW_GAP: i32 = 2;
/// 横排候选块之间的间距
pub const CAND_GAP: i32 = 12;

/// caret 下方与窗口之间的间隙（px）。
pub const CARET_GAP: i32 = 2;

/// 行矩形（窗口客户区坐标）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 工作区/显示器矩形（逻辑像素坐标，left/top/right/bottom 语义同 windows `RECT`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Area {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// 候选行显示文本：原文兜底候选（text == 预编辑原文去 `'`）不编号——
/// "不认识"语义，候选窗只呈现原文，不是可数候选；正常候选 `"N.候选"`。
/// layout 与 render 共用本规则，保证测量与绘制一致。
pub(crate) fn candidate_label(snap: &UiSnapshot, index: usize, cand: &str) -> String {
    if cand == snap.reading.replace('\'', "") {
        cand.to_string()
    } else {
        format!("{}.{}", index + 1, cand)
    }
}

/// 纯布局计算：返回 `(窗口宽, 窗口高, 候选矩形列表)`。
/// 竖排：每候选一行（`"N.候选"`），页码（`page_count > 1` 时）右对齐末行；
/// 横排：所有候选单行从左到右，页码在行尾右侧。
/// `snap.reading`（拼音分段）不渲染：composition 已显示，候选窗只放候选列表
/// （微软同款，省一行高度）。`measurer` 测量候选（主字体）、`page_measurer`
/// 测量页码（小字号）——页码用独立小字体测量，宽度/对齐才准确。
///
/// 测量器按值传 `FnMut`（比原 GDI 实现的 `&dyn Fn` 放宽一格）：无状态测量器
/// （`fn` 项 / `&fn` / 不捕获的闭包）与有状态测量器（cosmic-text 需 `&mut`）均可。
pub fn layout<F1, F2>(
    snap: &UiSnapshot,
    mut measurer: F1,
    mut page_measurer: F2,
    orientation: Orientation,
) -> (i32, i32, Vec<Rect>)
where
    F1: FnMut(&str) -> (i32, i32),
    F2: FnMut(&str) -> (i32, i32),
{
    let mut items: Vec<(String, i32, i32)> = Vec::new();
    for (i, cand) in snap.candidates.iter().enumerate() {
        let text = candidate_label(snap, i, cand);
        let (w, h) = measurer(&text);
        items.push((text, w, h));
    }
    let show_page = snap.page.page_count > 1;
    if show_page {
        let text = format!("{}/{}", snap.page.page + 1, snap.page.page_count);
        let (w, h) = page_measurer(&text);
        items.push((text, w, h));
    }
    if items.is_empty() {
        return (PAD_X * 2, PAD_Y * 2, Vec::new());
    }
    match orientation {
        Orientation::Vertical => {
            let content_w = items.iter().map(|r| r.1).max().unwrap_or(0);
            let mut rects = Vec::with_capacity(items.len());
            let mut y = PAD_Y;
            for (_, w, h) in &items {
                rects.push(Rect {
                    x: PAD_X,
                    y,
                    w: *w,
                    h: *h,
                });
                y += h + ROW_GAP;
            }
            if show_page {
                if let Some(last) = rects.last_mut() {
                    last.x = PAD_X + content_w - last.w;
                }
            }
            (content_w + PAD_X * 2, y - ROW_GAP + PAD_Y, rects)
        }
        Orientation::Horizontal => {
            // 候选单行：x 递增（候选间留 CAND_GAP）；页码在行尾右侧。
            let mut rects = Vec::with_capacity(items.len());
            let mut x = PAD_X;
            let mut row_h = 0i32;
            for (_, w, h) in &items {
                rects.push(Rect {
                    x,
                    y: PAD_Y,
                    w: *w,
                    h: *h,
                });
                row_h = row_h.max(*h);
                x += w + CAND_GAP;
            }
            let width = x - CAND_GAP + PAD_X;
            (width, row_h + PAD_Y * 2, rects)
        }
    }
}

/// 命中测试：坐标 (x,y)（窗口客户区）落在哪个候选矩形上。
/// 竖排/横排统一（layout 输出的候选矩形列表）。未命中返回 None。
pub fn hit_test(rects: &[Rect], x: i32, y: i32) -> Option<usize> {
    rects
        .iter()
        .position(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
}

/// update 原位修正：候选内容变化导致窗口变高/变宽时，
/// - 当前位置 + 新高度超出工作区底 → 用最近一次 caret 重新定位（下方放不下自动翻到光标上方）
/// - 当前位置 + 新宽度超出工作区右缘 → 左移内收，保证完整可见
/// 无 caret 兜底贴工作区底；未超屏保持原位。
pub fn update_position(
    current: (i32, i32),
    w: i32,
    h: i32,
    work: Area,
    last_caret: Option<CaretRect>,
) -> (i32, i32) {
    let (x, y) = current;
    if y + h <= work.bottom && x + w <= work.right {
        return (x, y);
    }
    match last_caret {
        Some(caret) => position_in_area(caret, w, h, work),
        None => {
            let x = if x + w > work.right {
                work.right - w
            } else {
                x
            };
            let y = if y + h > work.bottom {
                work.bottom - h
            } else {
                y
            };
            (x, y)
        }
    }
}

/// 按 caret 定位：默认放在 caret 下方；超出工作区则右/下边界内收，
/// 下方放不下时翻到 caret 上方。
///
/// 注意：本纯函数只做几何计算，不查显示器——显示器归属（caret 所在监视器的
/// 物理工作区，`MonitorFromPoint` + `GetMonitorInfoW`）由平台层（iuv-tsf）
/// 查好传入 `Area`，与 GDI 实现行为一致。
pub fn position_for(caret: CaretRect, w: i32, h: i32) -> (i32, i32) {
    // 无显示器可用时兜底近乎全屏区域（语义同 GDI 实现：SPI_GETWORKAREA 失败路径）。
    position_in_area(
        caret,
        w,
        h,
        Area {
            left: 0,
            top: 0,
            right: 32767,
            bottom: 32767,
        },
    )
}

/// 纯函数定位：给定工作区 `area` 内计算窗口位置。
/// 默认 `caret` 下方（光标底 + CARET_GAP）；右/下边界内收；下方放不下翻到 `caret` 上方；
/// 上下都放不下时贴工作区边，保证窗口完整可见。
pub fn position_in_area(caret: CaretRect, w: i32, h: i32, area: Area) -> (i32, i32) {
    let mut x = caret.x;
    if x + w > area.right {
        x = area.right - w;
    }
    if x < area.left {
        x = area.left;
    }
    // caret.h=0 时（collapsed 光标）同样按 CARET_GAP 留间隙。
    let below = caret.y + caret.h + CARET_GAP;
    let mut y = if below + h <= area.bottom {
        below
    } else {
        caret.y - h // 下方放不下 → 翻到 caret 上方
    };
    if y < area.top {
        y = area.top;
    }
    if y > area.bottom - h {
        y = area.bottom - h; // 上下都不够 → 贴底，窗口完整可见
    }
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::PageInfo;

    fn fake_measurer(s: &str) -> (i32, i32) {
        (s.chars().count() as i32 * 10, 20)
    }

    fn snap(reading: &str, candidates: &[&str], page: usize, page_count: usize) -> UiSnapshot {
        UiSnapshot {
            reading: reading.to_string(),
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            all_candidates: candidates.iter().map(|s| s.to_string()).collect(),
            selected: 0,
            page: PageInfo {
                page,
                page_count,
                page_size: 5,
                total: page_count * 5,
            },
            orientation: Orientation::Vertical,
        }
    }

    #[test]
    fn layout_single_page_rows_and_size() {
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 1);
        let (w, h, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert_eq!(rects.len(), 2, "2 候选，reading 不渲染");
        assert_eq!(w, 40 + PAD_X * 2, "最宽行 '1.你好'=4 字");
        assert_eq!(h, PAD_Y * 2 + 20 * 2 + ROW_GAP * 1);
        assert_eq!(
            rects[0],
            Rect {
                x: PAD_X,
                y: PAD_Y,
                w: 40,
                h: 20
            }
        );
        assert_eq!(rects[1].x, PAD_X);
        assert_eq!(rects[1].y, PAD_Y + (20 + ROW_GAP) * 1);
    }

    #[test]
    fn layout_multi_page_indicator_right_aligned() {
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 3);
        let (w, _, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert_eq!(rects.len(), 3, "2 候选 + 页码");
        let page_rect = *rects.last().unwrap();
        assert_eq!(
            page_rect.x,
            PAD_X + 40 - 30,
            "页码右对齐：x = PAD_X + content_w - 页码宽"
        );
        assert_eq!(page_rect.y, PAD_Y + (20 + ROW_GAP) * 2);
        assert_eq!(w, 40 + PAD_X * 2, "页码窄于最宽行，宽度不变");
    }

    #[test]
    fn layout_page_indicator_wider_than_rows() {
        let s = snap("ni", &["你"], 0, 100);
        let (w, _, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Vertical);
        let page_rect = *rects.last().unwrap();
        assert_eq!(w, 50 + PAD_X * 2, "页码 '1/100'=5 字 50px 最宽，撑开窗口");
        assert_eq!(page_rect.x, PAD_X, "页码自己最宽时从 PAD_X 起");
    }

    #[test]
    fn layout_page_uses_small_measurer() {
        // 页码用独立小测量（page_measurer）：5px/字 → '1/100' = 25px，而非主测量 50px。
        let s = snap("ni'hao", &["你好"], 0, 100);
        let fake_small = |t: &str| (t.chars().count() as i32 * 5, 10);
        let (w, _, rects) = layout(&s, &fake_measurer, &fake_small, Orientation::Vertical);
        let page_rect = *rects.last().unwrap();
        assert_eq!(page_rect.w, 25, "页码用 page_measurer 测量");
        assert_eq!(w, 40 + PAD_X * 2, "页码 25 < 候选 40，窗口宽由候选决定");
    }

    #[test]
    fn layout_empty_snapshot_no_rows() {
        let s = UiSnapshot::default();
        let (w, h, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert!(rects.is_empty());
        assert_eq!(w, PAD_X * 2);
        assert_eq!(h, PAD_Y * 2);
    }

    #[test]
    fn layout_ignores_reading() {
        // reading（拼音分段）不参与布局：composition 已显示，候选窗只放候选。
        let s = snap("ni'hao", &["你好"], 0, 1);
        let (_, _, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].y, PAD_Y);
        let s2 = snap("", &["你好"], 0, 1);
        let (_, _, rects2) = layout(&s2, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert_eq!(rects2.len(), 1, "有/无 reading 布局一致");
    }

    #[test]
    fn layout_fallback_raw_candidate_unnumbered() {
        // 原文兜底候选（text == reading 去撇号）不编号：测量文本是原文本身而非 "1.原文"。
        let s = snap("i'n'pu't", &["input"], 0, 1);
        let (w, _, _) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert_eq!(
            w,
            5 * 10 + PAD_X * 2,
            "无编号：宽 = 原文 5 字 × 10px + padding"
        );
        let s2 = snap("input", &["input"], 0, 1);
        let (w2, _, _) = layout(&s2, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert_eq!(w, w2, "reading 有无撇号判定等价");
        let s3 = snap("ni'hao", &["你好"], 0, 1);
        let (w3, _, _) = layout(&s3, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert_eq!(w3, 4 * 10 + PAD_X * 2, "正常候选仍编号 '1.你好'=4 字");
    }

    #[test]
    fn layout_candidate_widths() {
        let s = snap("ni", &["你好", "泥嚎"], 0, 1);
        let (w, _, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Vertical);
        assert_eq!(w, 40 + PAD_X * 2, "候选行 '1.你好'=4 字 40px 最宽");
        assert_eq!(rects[1].x, PAD_X);
    }

    #[test]
    fn layout_horizontal_single_row() {
        // 横排：候选单行从左到右，页码在行尾右侧。
        let s = snap("ni'hao", &["你好", "泥嚎", "你好吗"], 0, 2);
        let (w, h, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Horizontal);
        assert_eq!(rects.len(), 4, "3 候选 + 页码");
        // 候选矩形同一行（y=PAD_Y），x 递增
        assert_eq!(rects[0].y, PAD_Y);
        assert_eq!(rects[1].y, PAD_Y);
        assert_eq!(rects[2].y, PAD_Y);
        assert_eq!(rects[1].x, rects[0].x + rects[0].w + CAND_GAP);
        assert_eq!(rects[2].x, rects[1].x + rects[1].w + CAND_GAP);
        // 页码在行尾右侧（最后一个候选之后）
        assert!(rects[3].x > rects[2].x + rects[2].w);
        // 窗口宽 = 全部块宽 + 间距 + PAD；高 = 单行高 + PAD*2
        let expect_w = rects.iter().map(|r| r.w).sum::<i32>() + CAND_GAP * 3 + PAD_X * 2;
        assert_eq!(w, expect_w);
        assert_eq!(h, 20 + PAD_Y * 2);
    }

    #[test]
    fn hit_test_vertical_rows() {
        let s = snap("ni'hao", &["你好", "泥嚎", "你好吗"], 0, 1);
        let (_, _, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Vertical);
        // 命中各行：矩形左上角 / 右下角内侧
        assert_eq!(hit_test(&rects, rects[0].x, rects[0].y), Some(0));
        assert_eq!(
            hit_test(
                &rects,
                rects[1].x + rects[1].w - 1,
                rects[1].y + rects[1].h - 1
            ),
            Some(1)
        );
        // 行间 gap：未命中
        assert_eq!(hit_test(&rects, PAD_X, rects[0].y + rects[0].h + 1), None);
        // 越界：未命中
        assert_eq!(hit_test(&rects, -1, 0), None);
        assert_eq!(hit_test(&rects, 0, 9999), None);
    }

    #[test]
    fn hit_test_horizontal_blocks() {
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 2);
        let (_, _, rects) = layout(&s, &fake_measurer, &fake_measurer, Orientation::Horizontal);
        // 横排：命中各候选块；块间 gap 未命中（页码块不计入候选）
        assert_eq!(hit_test(&rects, rects[0].x + 1, rects[0].y + 1), Some(0));
        assert_eq!(hit_test(&rects, rects[1].x + 1, rects[1].y + 1), Some(1));
        assert_eq!(
            hit_test(&rects, rects[0].x + rects[0].w + 1, rects[0].y + 1),
            None,
            "候选块之间间距未命中"
        );
    }

    #[test]
    fn update_position_keeps_in_place_when_fits() {
        let work = Area {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let c = CaretRect {
            x: 100,
            y: 700,
            w: 2,
            h: 20,
        };
        assert_eq!(
            update_position((100, 800), 200, 195, work, Some(c)),
            (100, 800),
            "当前位置 + 新高度不超屏 → 保持原位"
        );
    }

    #[test]
    fn update_position_flips_above_caret_when_overflow() {
        let work = Area {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 900,
        };
        let c = CaretRect {
            x: 100,
            y: 800,
            w: 2,
            h: 20,
        };
        assert_eq!(
            update_position((100, 800), 200, 195, work, Some(c)),
            (100, 605),
            "窗口变高超屏 → 用 caret 重定位：下方放不下翻到光标上方"
        );
    }

    #[test]
    fn update_position_clamps_right_edge_when_wider() {
        let work = Area {
            left: 0,
            top: 0,
            right: 3138,
            bottom: 900,
        };
        let c = CaretRect {
            x: 3043,
            y: 757,
            w: 2,
            h: 20,
        };
        // 窗口变宽到 237：3043+237=3280 > 3138 → 左移内收，右缘对齐工作区。
        // y 按 caret 重定位：779 = 757+20+2（光标下方，不超底）。
        assert_eq!(
            update_position((3043, 562), 237, 60, work, Some(c)),
            (3138 - 237, 779),
            "变宽超右缘 → 左移内收，右缘对齐工作区"
        );
    }

    #[test]
    fn update_position_clamps_to_work_bottom_without_caret() {
        let work = Area {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 900,
        };
        assert_eq!(
            update_position((100, 800), 200, 195, work, None),
            (100, 900 - 195),
            "无 caret 锚点兜底 → 贴工作区底，保证完整可见"
        );
    }

    #[test]
    fn position_below_caret_by_default() {
        let area = Area {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let caret = CaretRect {
            x: 100,
            y: 100,
            w: 2,
            h: 20,
        };
        let (x, y) = position_in_area(caret, 200, 100, area);
        assert_eq!(
            (x, y),
            (100, 122),
            "默认 caret 正下方（光标底 + 2px 间隙），不越界原样保留"
        );
    }

    #[test]
    fn position_flips_above_caret_when_no_room_below() {
        let area = Area {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        // caret 贴近屏幕底边：窗口应翻到 caret 上方
        let caret = CaretRect {
            x: 100,
            y: 1000,
            w: 2,
            h: 20,
        };
        let (x, y) = position_in_area(caret, 200, 100, area);
        assert_eq!(y, caret.y - 100);
        assert_eq!(x, caret.x, "x 未越界保持不变");
    }

    #[test]
    fn position_clamps_into_workarea() {
        let area = Area {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        // caret 在右/下边缘：窗口右/下边界内收
        let caret = CaretRect {
            x: 1900,
            y: 1000,
            w: 2,
            h: 20,
        };
        let (x, y) = position_in_area(caret, 300, 200, area);
        assert_eq!(x, 1920 - 300, "右边界内收");
        assert_eq!(y, 1000 - 200, "下方放不下翻到上方");
        assert!(x + 300 <= area.right);
        assert!(y + 200 <= area.bottom);
    }

    #[test]
    fn position_clamps_to_area_edge_when_caret_fully_outside() {
        let area = Area {
            left: 100,
            top: 100,
            right: 1900,
            bottom: 1000,
        };
        // caret 完全在工作区外且上方也放不下 → 贴底，窗口完整可见
        let caret = CaretRect {
            x: 50,
            y: 5000,
            w: 2,
            h: 20,
        };
        let (x, y) = position_in_area(caret, 300, 200, area);
        assert_eq!(x, area.left, "左边界内收");
        assert_eq!(y, area.bottom - 200, "贴工作区底");
        assert!(y >= area.top);
    }

    #[test]
    fn position_clamps_to_area_top_when_caret_fully_above() {
        let area = Area {
            left: 0,
            top: 100,
            right: 1920,
            bottom: 1040,
        };
        // caret 在工作区上方：贴工作区顶
        let caret = CaretRect {
            x: 100,
            y: -100,
            w: 2,
            h: 20,
        };
        let (_, y) = position_in_area(caret, 200, 100, area);
        assert_eq!(y, area.top);
    }

    #[test]
    fn position_without_caret_height_keeps_small_gap() {
        let area = Area {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let caret = CaretRect {
            x: 100,
            y: 100,
            w: 0,
            h: 0,
        };
        let (_, y) = position_in_area(caret, 200, 100, area);
        assert_eq!(y, caret.y + 2, "无 caret 高度时留 2px 间隙");
    }
}
