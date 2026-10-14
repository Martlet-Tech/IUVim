//! 桌宠挂载档案（54-2 Step 5）：记住"挂的是哪个窗口"，重启后按身份找回。
//!
//! ## 匹配规则（真机实测逼出来的，不是拍脑袋）
//! 主键 = **进程路径 + 类名**；尺寸**只作消歧提示，不作硬条件**。
//! 依据：BongoCat 实测窗口尺寸会变（350×203 → 280×162，正好 1.25 倍缩放）——
//! 拿尺寸当键，用户一改缩放就会"找不到挂过的宠物"。
//!
//! 同构多窗口（Shimeji 一群 mascot）无法稳定锁定某一只：消歧取"尺寸最接近者"，
//! 并列取 z 序最上层。**这是 MVP 已知边界**（用户侧把 Shimeji 设成单只即可规避）。
//!
//! ## 持久化
//! `%LOCALAPPDATA%\iuv\mount.json`（与 `toolbar.json` 同目录）。写入用
//! "临时文件 + rename 原子替换"（与 toolbar prefs 同套路）；读写失败一律静默降级
//! （记日志），绝不阻断主流程。

use std::path::PathBuf;

use crate::logger::log_line;
use crate::window_probe::{list_candidates, ProbeRect, WindowProbe};

/// 档案文件名（`%LOCALAPPDATA%\iuv\` 下）。
const FILE_NAME: &str = "mount.json";

/// 挂载档案：身份（本步 + Step 5）+ 姿态与偏移（Step 6）。
/// 新增字段一律配 `#[serde(default)]`，保证 Step 5 时期写下的旧档案仍可读。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MountProfile {
    /// 进程完整路径（主键之一）
    pub process_path: String,
    /// 窗口类名（主键之二）
    pub class_name: String,
    /// 捕捉时的窗口尺寸 `[w, h]`——**仅作消歧提示**
    pub size_hint: [i32; 2],
    /// 捕捉时的扩展样式（诊断 / 回退判定用）
    pub ex_style: u32,
    /// 锚点对 `[宠物锚点, 工具栏锚点]`（9 宫格索引）。缺省 = 默认姿态「右上外侧」。
    #[serde(default = "default_anchor")]
    pub anchor: [u8; 2],
    /// 从**宠物锚点**到**工具栏锚点**的偏移（px，含贴边间距）。缺省 = 8px 间距。
    #[serde(default = "default_offset")]
    pub offset: [i32; 2],
}

/// 默认姿态：宠物**右上角** ↔ 工具栏**左上角**（即"右上外侧"）。
pub fn default_anchor() -> [u8; 2] {
    [ANCHOR_TOP_RIGHT, ANCHOR_TOP_LEFT]
}

/// 默认间距：右侧外挂 8px、顶对齐。
pub fn default_offset() -> [i32; 2] {
    [8, 0]
}

impl MountProfile {
    /// 可执行文件名（日志 / 设置页展示用）。
    pub fn exe_name(&self) -> &str {
        self.process_path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&self.process_path)
    }
}

/// 档案路径（`%LOCALAPPDATA%\iuv\mount.json`）。
pub fn profile_path() -> Option<PathBuf> {
    iuv_core::iuv_dir().map(|d| d.join(FILE_NAME))
}

// ============================================================
// 锚点与跟随几何（**纯函数**，不碰 Win32）
// ------------------------------------------------------------
// 模型（54-2 §2，冻结）：
//     desired = petAnchor + offset − barAnchorLocal
//     visual  = clamp(desired, 可用区域)          ← 分轴
// `offset` 是唯一真相源，唯一写入口是用户拖拽；夹紧**从不回写** offset，
// 故宠物回到"开始被夹住时的位置"会自动恢复精确跟踪（无状态，不记历史）。
// ============================================================

/// 9 宫格锚点索引（行优先）：0=左上 1=上中 2=右上 3=左中 4=中心 5=右中 6=左下 7=下中 8=右下。
pub const ANCHOR_TOP_LEFT: u8 = 0;
pub const ANCHOR_TOP_RIGHT: u8 = 2;
pub const ANCHOR_CENTER: u8 = 4;
pub const ANCHOR_BOTTOM_LEFT: u8 = 6;
pub const ANCHOR_BOTTOM_RIGHT: u8 = 8;

/// 9 宫格索引 → `(col, row)` 各自 0/1/2。
const fn grid_parts(idx: u8) -> (i32, i32) {
    ((idx % 3) as i32, (idx / 3) as i32)
}

/// 由 `(col, row)` 组 9 宫格索引。
const fn grid_idx(col: i32, row: i32) -> u8 {
    (row * 3 + col) as u8
}

/// 矩形上某宫格点的坐标。
pub fn anchor_point(r: ProbeRect, idx: u8) -> (i32, i32) {
    let (col, row) = grid_parts(idx);
    let x = r.x
        + if col == 0 {
            0
        } else if col == 1 {
            r.w / 2
        } else {
            r.w
        };
    let y = r.y
        + if row == 0 {
            0
        } else if row == 1 {
            r.h / 2
        } else {
            r.h
        };
    (x, y)
}

/// 工具栏的**期望**落点（未夹紧）：`petAnchor + offset − barAnchorLocal`。
pub fn desired_pos(
    pet: ProbeRect,
    bar_w: i32,
    bar_h: i32,
    anchor: [u8; 2],
    offset: [i32; 2],
) -> (i32, i32) {
    let (px, py) = anchor_point(pet, anchor[0]);
    let bar = ProbeRect {
        x: 0,
        y: 0,
        w: bar_w,
        h: bar_h,
    };
    let (bx, by) = anchor_point(bar, anchor[1]);
    (px + offset[0] - bx, py + offset[1] - by)
}

/// 分轴夹紧到可用区域（`area` = 虚拟桌面矩形）。窗口比区域还大时贴左上。
pub fn clamp_pos(pos: (i32, i32), bar_w: i32, bar_h: i32, area: ProbeRect) -> (i32, i32) {
    let max_x = (area.right() - bar_w).max(area.x);
    let max_y = (area.bottom() - bar_h).max(area.y);
    (pos.0.clamp(area.x, max_x), pos.1.clamp(area.y, max_y))
}

/// 拖拽松手后**由视觉位置反推** offset（拖拽是 offset 的唯一写入口）。
pub fn offset_from_visual(
    bar_pos: (i32, i32),
    bar_w: i32,
    bar_h: i32,
    pet: ProbeRect,
    anchor: [u8; 2],
) -> [i32; 2] {
    let (px, py) = anchor_point(pet, anchor[0]);
    let bar = ProbeRect {
        x: 0,
        y: 0,
        w: bar_w,
        h: bar_h,
    };
    let (bx, by) = anchor_point(bar, anchor[1]);
    [bar_pos.0 + bx - px, bar_pos.1 + by - py]
}

/// 按方位**重判锚点对**（松手时调用）：把工具栏吸到宠物"最近那条边"的外侧。
///
/// 规则（简单可预测）：主轴 = `|dx|` 与 `|dy|` 较大者（dx/dy = 工具栏中心 − 宠物中心）；
/// 主轴为水平 → 取右/左边（宠物边缘 ↔ 工具栏对边）；副轴按中心落在宠物三等分取 上/中/下。
/// 这样宠物改尺寸时，工具栏仍贴着"同一条边"，而不是跟着对侧边缘乱跑。
pub fn judge_anchor(pet: ProbeRect, bar_pos: (i32, i32), bar_w: i32, bar_h: i32) -> [u8; 2] {
    let dx = (bar_pos.0 + bar_w / 2) - (pet.x + pet.w / 2);
    let dy = (bar_pos.1 + bar_h / 2) - (pet.y + pet.h / 2);
    // 三等分：中心落在哪一段（0/1/2）
    let seg = |center: i32, lo: i32, len: i32| -> i32 {
        if len <= 0 {
            return 1;
        }
        let rel = center - lo;
        if rel < len / 3 {
            0
        } else if rel < len * 2 / 3 {
            1
        } else {
            2
        }
    };
    if dx.abs() >= dy.abs() {
        let row = seg(bar_pos.1 + bar_h / 2, pet.y, pet.h);
        if dx >= 0 {
            [grid_idx(2, row), grid_idx(0, row)] // 宠物右缘 ↔ 工具栏左缘
        } else {
            [grid_idx(0, row), grid_idx(2, row)]
        }
    } else {
        let col = seg(bar_pos.0 + bar_w / 2, pet.x, pet.w);
        if dy >= 0 {
            [grid_idx(col, 2), grid_idx(col, 0)] // 宠物下缘 ↔ 工具栏上缘
        } else {
            [grid_idx(col, 0), grid_idx(col, 2)]
        }
    }
}

/// 一次跟随求解：目标矩形 + 工具栏尺寸 + 档案姿态 → 应落位置（已夹紧）。
pub fn follow_pos(
    pet: ProbeRect,
    bar_w: i32,
    bar_h: i32,
    profile: &MountProfile,
    area: ProbeRect,
) -> (i32, i32) {
    let desired = desired_pos(pet, bar_w, bar_h, profile.anchor, profile.offset);
    clamp_pos(desired, bar_w, bar_h, area)
}

/// 由捕捉结果建档案（姿态/偏移用默认值，用户拖拽后由 `offset_from_visual` 覆盖）。
pub fn from_probe(p: &WindowProbe) -> MountProfile {
    MountProfile {
        process_path: p.process_path.clone(),
        class_name: p.class_name.clone(),
        size_hint: [p.rect.w, p.rect.h],
        ex_style: p.ex_style,
        anchor: default_anchor(),
        offset: default_offset(),
    }
}

/// 加载档案。缺失 / 解析失败 / 主键为空 → None（静默降级）。
pub fn load() -> Option<MountProfile> {
    let path = profile_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<MountProfile>(&text) {
        Ok(p) if !p.process_path.is_empty() && !p.class_name.is_empty() => Some(p),
        Ok(_) => {
            log_line("[mount] 档案主键为空，忽略");
            None
        }
        Err(e) => {
            log_line(&format!("[mount] 档案解析失败：{e}"));
            None
        }
    }
}

/// 保存档案（临时文件 + rename 原子替换）。返回是否成功。
pub fn save(p: &MountProfile) -> bool {
    let Some(path) = profile_path() else {
        return false;
    };
    let Ok(text) = serde_json::to_string_pretty(p) else {
        log_line("[mount] 档案序列化失败");
        return false;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, text) {
        log_line(&format!("[mount] 档案写入失败：{e}"));
        return false;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        log_line(&format!("[mount] 档案替换失败：{e}"));
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

/// 清除档案（解绑）。返回是否确实删掉了文件。
pub fn clear() -> bool {
    let Some(path) = profile_path() else {
        return false;
    };
    std::fs::remove_file(&path).is_ok()
}

/// 进程路径比较（Windows 路径大小写不敏感）。
fn same_process(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// 尺寸距离（越小越接近）。`hint` 无效（≤0）→ 恒 0，即退化为"取 z 序最上层"。
fn size_distance(w: i32, h: i32, hint: [i32; 2]) -> i64 {
    if hint[0] <= 0 || hint[1] <= 0 {
        return 0;
    }
    (i64::from(w) - i64::from(hint[0])).abs() + (i64::from(h) - i64::from(hint[1])).abs()
}

/// 纯选择：主键过滤 + 尺寸最近；**并列取先出现者**（输入顺序 = z 序自顶向下）。
/// 返回命中下标。可单测（不碰 Win32）。
fn pick_profile_match(cands: &[WindowProbe], profile: &MountProfile) -> Option<usize> {
    let mut best: Option<(usize, i64)> = None;
    for (i, p) in cands.iter().enumerate() {
        if !same_process(&p.process_path, &profile.process_path)
            || p.class_name != profile.class_name
        {
            continue;
        }
        let d = size_distance(p.rect.w, p.rect.h, profile.size_hint);
        match best {
            // 严格小于才替换 → 并列时保留先出现者（z 序更上层）
            Some((_, bd)) if d >= bd => {}
            _ => best = Some((i, d)),
        }
    }
    best.map(|(i, _)| i)
}

/// 按档案找回目标：枚举候选 → 主键过滤 → 尺寸最近（并列取 z 序最上层）。
/// 未命中（宠物没在跑 / 已卸载）→ None。
pub fn find_target(profile: &MountProfile) -> Option<WindowProbe> {
    let cands = list_candidates();
    pick_profile_match(&cands, profile).map(|i| cands[i].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_probe::{Mountability, ProbeRect};

    fn probe(path: &str, class: &str, w: i32, h: i32) -> WindowProbe {
        WindowProbe {
            hwnd: 0,
            class_name: class.to_owned(),
            process_path: path.to_owned(),
            title: String::new(),
            rect: ProbeRect { x: 0, y: 0, w, h },
            ex_style: 0,
            mountability: Mountability::Ok,
        }
    }

    fn profile(path: &str, class: &str, hint: [i32; 2]) -> MountProfile {
        MountProfile {
            process_path: path.to_owned(),
            class_name: class.to_owned(),
            size_hint: hint,
            ex_style: 0,
            anchor: default_anchor(),
            offset: default_offset(),
        }
    }

    #[test]
    fn match_requires_both_keys() {
        let cands = vec![
            probe("C:\\a\\bongocat.exe", "Other", 100, 100),
            probe("C:\\a\\other.exe", "BongoCatOverlay", 100, 100),
            probe("C:\\a\\bongocat.exe", "BongoCatOverlay", 100, 100),
        ];
        // 路径或类名任一不符都不算命中
        assert_eq!(
            pick_profile_match(
                &cands,
                &profile("C:\\a\\bongocat.exe", "BongoCatOverlay", [100, 100])
            ),
            Some(2)
        );
        assert_eq!(
            pick_profile_match(&cands, &profile("C:\\a\\bongocat.exe", "Nope", [100, 100])),
            None
        );
    }

    #[test]
    fn path_compare_is_case_insensitive() {
        let cands = vec![probe("C:\\Apps\\BongoCat.exe", "W", 10, 10)];
        assert_eq!(
            pick_profile_match(&cands, &profile("c:\\apps\\bongocat.exe", "W", [10, 10])),
            Some(0),
            "Windows 路径大小写不敏感"
        );
    }

    #[test]
    fn size_is_hint_not_hard_key() {
        // 宠物改了缩放（350×203 → 280×162）：尺寸不符仍必须找回
        let cands = vec![probe("C:\\bongo.exe", "Overlay", 280, 162)];
        assert_eq!(
            pick_profile_match(&cands, &profile("C:\\bongo.exe", "Overlay", [350, 203])),
            Some(0),
            "尺寸变了也要找回——这是档案用尺寸当提示而非键的理由"
        );
    }

    #[test]
    fn nearest_size_wins_and_ties_keep_topmost() {
        // 输入顺序 = z 序自顶向下
        let cands = vec![
            probe("C:\\j\\javaw.exe", "SunAwtWindow", 131, 278), // 0：最上层，尺寸差 4+4=8
            probe("C:\\j\\javaw.exe", "SunAwtWindow", 127, 274), // 1：尺寸差 0 → 更近
        ];
        assert_eq!(
            pick_profile_match(
                &cands,
                &profile("C:\\j\\javaw.exe", "SunAwtWindow", [127, 274])
            ),
            Some(1),
            "尺寸更近者胜"
        );
        // 两者尺寸相同 → 并列，取先出现者（z 序最上层）
        let cands2 = vec![
            probe("C:\\j\\javaw.exe", "SunAwtWindow", 127, 274),
            probe("C:\\j\\javaw.exe", "SunAwtWindow", 127, 274),
        ];
        assert_eq!(
            pick_profile_match(
                &cands2,
                &profile("C:\\j\\javaw.exe", "SunAwtWindow", [127, 274])
            ),
            Some(0),
            "并列取 z 序最上层"
        );
    }

    #[test]
    fn invalid_hint_degrades_to_topmost() {
        // hint 无效 → 距离恒 0 → 退化为取最上层
        let cands = vec![
            probe("C:\\j\\javaw.exe", "SunAwtWindow", 500, 500),
            probe("C:\\j\\javaw.exe", "SunAwtWindow", 127, 274),
        ];
        assert_eq!(
            pick_profile_match(&cands, &profile("C:\\j\\javaw.exe", "SunAwtWindow", [0, 0])),
            Some(0)
        );
    }

    #[test]
    fn profile_roundtrips_through_json() {
        let p = profile(
            "C:\\Apps\\bongocat-app.exe",
            "BongoCatProductOverlayWindow",
            [280, 162],
        );
        let text = serde_json::to_string(&p).expect("序列化");
        let back: MountProfile = serde_json::from_str(&text).expect("反序列化");
        assert_eq!(p, back);
        assert_eq!(back.exe_name(), "bongocat-app.exe");
    }

    #[test]
    fn exe_name_splits_both_separators() {
        assert_eq!(profile("C:\\a\\b\\x.exe", "W", [1, 1]).exe_name(), "x.exe");
        assert_eq!(profile("/usr/bin/x", "W", [1, 1]).exe_name(), "x");
        assert_eq!(profile("x.exe", "W", [1, 1]).exe_name(), "x.exe");
    }

    // ---- 锚点与跟随几何 ----

    fn rect(x: i32, y: i32, w: i32, h: i32) -> ProbeRect {
        ProbeRect { x, y, w, h }
    }

    /// 1920×1080 虚拟桌面
    fn area() -> ProbeRect {
        rect(0, 0, 1920, 1080)
    }

    /// 工具栏尺寸（7 按钮 @96dpi）
    const BAR: (i32, i32) = (246, 30);

    #[test]
    fn anchor_point_covers_nine_grid() {
        let r = rect(100, 200, 60, 40);
        assert_eq!(anchor_point(r, 0), (100, 200), "左上");
        assert_eq!(anchor_point(r, 1), (130, 200), "上中");
        assert_eq!(anchor_point(r, 2), (160, 200), "右上");
        assert_eq!(anchor_point(r, 3), (100, 220), "左中");
        assert_eq!(anchor_point(r, 4), (130, 220), "中心");
        assert_eq!(anchor_point(r, 5), (160, 220), "右中");
        assert_eq!(anchor_point(r, 6), (100, 240), "左下");
        assert_eq!(anchor_point(r, 7), (130, 240), "下中");
        assert_eq!(anchor_point(r, 8), (160, 240), "右下");
    }

    #[test]
    fn default_pose_is_right_side_top_aligned() {
        let pet = rect(100, 100, 200, 150);
        let p = profile("a.exe", "W", [200, 150]);
        // 默认「右上外侧」：工具栏左上角落在宠物右上角右侧 8px、顶对齐
        assert_eq!(
            desired_pos(pet, BAR.0, BAR.1, p.anchor, p.offset),
            (308, 100)
        );
        // 且默认姿态就是"工具栏在右、顶对齐"时的重判结果（两者必须一致）
        assert_eq!(judge_anchor(pet, (308, 100), BAR.0, BAR.1), p.anchor);
    }

    #[test]
    fn clamp_is_per_axis_and_slides_along_edge() {
        // 宠物贴屏幕底部、工具栏挂在它**下方外侧** → 工具栏触底
        let mut p = profile("a.exe", "W", [200, 150]);
        p.anchor = [7, 1]; // 宠物下中 ↔ 工具栏上中
        p.offset = [0, 8];
        let pet = rect(100, 900, 200, 150);
        let (x1, y1) = follow_pos(pet, BAR.0, BAR.1, &p, area());
        assert_eq!(y1, 1080 - BAR.1, "y 被夹到可用区域底边");
        // 宠物沿底边向右 → y 不动、x 继续跟随 = **沿边滑行**（分轴夹紧的免费好处）
        let pet2 = rect(400, 900, 200, 150);
        let (x2, y2) = follow_pos(pet2, BAR.0, BAR.1, &p, area());
        assert_eq!(y2, 1080 - BAR.1, "y 仍被夹住");
        assert_eq!(x2 - x1, 300, "x 与宠物位移一致 → 沿底边滑行");
        // 右边缘同理
        let pet3 = rect(1800, 900, 200, 150);
        let (x3, _) = follow_pos(pet3, BAR.0, BAR.1, &p, area());
        assert_eq!(x3, 1920 - BAR.0, "x 被夹到可用区域右边");
    }

    #[test]
    fn clamp_never_writes_back_offset() {
        let p = profile("a.exe", "W", [200, 150]);
        let home = rect(100, 100, 200, 150);
        let far = rect(1800, 100, 200, 150); // 宠物跑到最右 → 工具栏被右边缘夹住
        assert_eq!(follow_pos(home, BAR.0, BAR.1, &p, area()), (308, 100));
        let clamped = follow_pos(far, BAR.0, BAR.1, &p, area());
        assert_eq!(clamped.0, 1920 - BAR.0, "被夹到右边");
        assert_ne!(
            clamped.0,
            desired_pos(far, BAR.0, BAR.1, p.anchor, p.offset).0,
            "确实发生了夹紧"
        );
        // 宠物回到原位 → **自动恢复精确跟踪**（无状态，offset 未被夹紧污染）
        assert_eq!(
            follow_pos(home, BAR.0, BAR.1, &p, area()),
            (308, 100),
            "不需要记住任何历史量"
        );
    }

    #[test]
    fn offset_from_visual_roundtrips() {
        let pet = rect(100, 100, 200, 150);
        let p = profile("a.exe", "W", [200, 150]);
        let pos = desired_pos(pet, BAR.0, BAR.1, p.anchor, p.offset);
        let back = offset_from_visual(pos, BAR.0, BAR.1, pet, p.anchor);
        assert_eq!(back, p.offset, "视觉位置反推 offset 应回到原值");
        // 用户拖到宠物左侧 → offset 变负，且能原样复现
        let moved = (-200, 120);
        let off = offset_from_visual(moved, BAR.0, BAR.1, pet, p.anchor);
        assert_eq!(desired_pos(pet, BAR.0, BAR.1, p.anchor, off), moved);
    }

    #[test]
    fn judge_anchor_picks_nearest_side() {
        let pet = rect(100, 100, 200, 150);
        // 右侧
        assert_eq!(judge_anchor(pet, (308, 100), BAR.0, BAR.1), [2, 0]);
        // 左侧
        assert_eq!(judge_anchor(pet, (-260, 100), BAR.0, BAR.1), [0, 2]);
        // 下方（居中）
        assert_eq!(judge_anchor(pet, (100, 300), BAR.0, BAR.1), [7, 1]);
        // 上方（居中）
        assert_eq!(judge_anchor(pet, (100, -60), BAR.0, BAR.1), [1, 7]);
        // 退化的宠物矩形：不 panic，回落到中段
        let _ = judge_anchor(rect(0, 0, 0, 0), (0, 0), BAR.0, BAR.1);
    }

    #[test]
    fn old_profile_without_pose_still_loads() {
        // Step 5 时期写下的档案（无 anchor/offset 字段）必须仍可读，且补上默认姿态
        let old =
            r#"{"process_path":"C:\\a\\x.exe","class_name":"W","size_hint":[10,20],"ex_style":0}"#;
        let p: MountProfile = serde_json::from_str(old).expect("旧档案应可读");
        assert_eq!(p.anchor, default_anchor());
        assert_eq!(p.offset, default_offset());
    }
}
