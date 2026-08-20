//! 工具栏图标（32-status-toolbar.md §6.7）：源图 `include_bytes!` 内嵌 + 运行时解码。
//!
//! 决策（2026-08-20）：**不建缩放/转换工具**——源图即最终素材（`assets/main.png` +
//! `assets/toolbar-icons/*.png`，~28-32px 近方形），tiny-skia 自带 PNG 解码
//! （`Pixmap::decode_png`）+ `draw_pixmap` 渲染层缩放（render_toolbar 内），改图即生效。
//! 编译期内嵌进 exe，零外部文件依赖；解码失败降级 None（按钮留空，不 panic）。

use tiny_skia::Pixmap;

use iuv_ui::ToolbarIcons;

fn decode(bytes: &[u8], name: &str) -> Option<Pixmap> {
    match Pixmap::decode_png(bytes) {
        Ok(p) => Some(p),
        Err(e) => {
            crate::log::log_line(&format!("[toolbar] 图标解码失败 {name}: {e}"));
            None
        }
    }
}

macro_rules! asset {
    ($file:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/", $file)
    };
}

/// 解码全部内嵌图标（进程启动一次；单个失败降级 None，整体仍返回）。
pub fn load_icons() -> ToolbarIcons {
    ToolbarIcons {
        logo: decode(include_bytes!(asset!("main.png")), "main.png"),
        lang_cn: decode(
            include_bytes!(asset!("toolbar-icons/lang-cn.png")),
            "lang-cn.png",
        ),
        lang_en: decode(
            include_bytes!(asset!("toolbar-icons/lang-en.png")),
            "lang-en.png",
        ),
        width_half: decode(
            include_bytes!(asset!("toolbar-icons/width-half.png")),
            "width-half.png",
        ),
        width_full: decode(
            include_bytes!(asset!("toolbar-icons/width-full.png")),
            "width-full.png",
        ),
        punct_cn: decode(
            include_bytes!(asset!("toolbar-icons/punctuate-cn.png")),
            "punctuate-cn.png",
        ),
        punct_en: decode(
            include_bytes!(asset!("toolbar-icons/punctuate-en.png")),
            "punctuate-en.png",
        ),
        script_simplified: decode(
            include_bytes!(asset!("toolbar-icons/script-simplified.png")),
            "script-simplified.png",
        ),
        script_traditional: decode(
            include_bytes!(asset!("toolbar-icons/script-traditional.png")),
            "script-traditional.png",
        ),
        gear: decode(
            include_bytes!(asset!("toolbar-icons/gear-settings.png")),
            "gear-settings.png",
        ),
    }
}
