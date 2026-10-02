//! Effect → UiSnapshot 映射与自绘菜单窗口。
//! （M10 后候选窗由 iuv-server 自渲染；本地 ULW 候选窗 CandwinCandidateWindow
//! 与 CandidateUi 抽象已于 2026-10-02 品质审查 D3 清扫——服务端渲染下
//! show/update 永不触达，约 600 行死路径 + 死测试移除，见 50 号 §3。）

pub mod menu_window;

pub use iuv_ui::{effect_to_snapshot, CaretRect, UiSnapshot};
pub use menu_window::MenuWindow;
