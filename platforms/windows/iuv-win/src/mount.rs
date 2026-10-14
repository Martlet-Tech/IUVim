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
use crate::window_probe::{list_candidates, WindowProbe};

/// 档案文件名（`%LOCALAPPDATA%\iuv\` 下）。
const FILE_NAME: &str = "mount.json";

/// 挂载档案：本步只记**身份**。锚点 / offset 是 Step 6 的事，届时追加字段并配
/// `#[serde(default)]`，保证旧档案仍可读。
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

/// 由捕捉结果建档案。
pub fn from_probe(p: &WindowProbe) -> MountProfile {
    MountProfile {
        process_path: p.process_path.clone(),
        class_name: p.class_name.clone(),
        size_hint: [p.rect.w, p.rect.h],
        ex_style: p.ex_style,
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
}
