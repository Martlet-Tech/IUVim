//! 持久化 `toolbar.json`（P2.6 自 toolbar.rs 拆出）：显示偏好 + 位置。
//! 全局唯一（daemon 唯一写者），独立文件不触发 config_epoch 热载噪声。

/// toolbar.json 内容（显示偏好 + 位置；全局，daemon 唯一写者，§6.3）。
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) struct ToolbarPref {
    #[serde(default = "default_visible")]
    pub(super) visible: bool,
    #[serde(default)]
    pub(super) pos: Option<(i32, i32)>,
}

fn default_visible() -> bool {
    true
}

/// %LOCALAPPDATA%\iuv\toolbar.json（独立文件：不触发 config_epoch 热载噪声）。
fn pref_path() -> Option<std::path::PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .or_else(|| std::env::var("APPDATA").ok().map(|a| format!("{a}\\Local")))
        .or_else(|| std::env::var("HOME").ok())?;
    Some(std::path::PathBuf::from(base).join("iuv").join("toolbar.json"))
}

/// 加载偏好（缺失/损坏 → 默认 visible=true、pos=None；绝不失败）。
/// 位置清洗（2026-08-21）：越界坐标（旧版本 32767 bug / 拖拽损坏 / 显示器拔除残留）
/// → 置 None（show 时用主屏右下角默认），避免工具栏渲染到屏幕外 = 隐形。
pub(super) fn load_pref() -> ToolbarPref {
    let Some(path) = pref_path() else {
        return ToolbarPref {
            visible: true,
            pos: None,
        };
    };
    match std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<ToolbarPref>(&t).ok())
    {
        Some(mut p) => {
            if let Some((x, y)) = p.pos {
                // 越界判据：明显超出 Win32 虚拟桌面合理范围（-10000..40000）。
                if x < -10000 || x > 40000 || y < -10000 || y > 40000 {
                    p.pos = None;
                }
            }
            p
        }
        None => ToolbarPref {
            visible: true,
            pos: None,
        },
    }
}

/// 保存偏好：临时文件 + 先删后 rename 原子替换；失败不阻断（内存态已生效）。
pub(super) fn save_pref(pref: &ToolbarPref) {
    let Some(path) = pref_path() else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join("toolbar.json.tmp");
    match serde_json::to_string_pretty(pref).map(|t| std::fs::write(&tmp, t)) {
        Ok(Ok(())) => {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::rename(&tmp, &path);
        }
        _ => {
            crate::log::log_line("[toolbar] 偏好写盘失败（内存态已生效）");
            let _ = std::fs::remove_file(&tmp);
        }
    }
}