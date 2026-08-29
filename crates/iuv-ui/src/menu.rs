//! 菜单数据模型与命中测试（M5 语言栏右键菜单用；托盘方案已于 2026-08-17 废弃）。
//!
//! `id` 由调用方自定义语义（M5 点击回调用）；**0 保留给分隔线**——
//! `render_menu` 对 id==0 的条目画分隔线而非文本。

use crate::layout::Rect;

/// 菜单条目。`id == 0` = 分隔线语义（render 画线，不画文本、不高亮）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: String,
    pub id: u16,
}

impl MenuEntry {
    /// 构造文本条目。
    pub fn new(label: impl Into<String>, id: u16) -> Self {
        MenuEntry {
            label: label.into(),
            id,
        }
    }

    /// 分隔线条目（id = 0，label 忽略）。
    pub fn separator() -> Self {
        MenuEntry {
            label: String::new(),
            id: 0,
        }
    }

    /// 是否分隔线（id == 0 保留语义）。
    pub fn is_separator(&self) -> bool {
        self.id == 0
    }
}

/// 菜单命中测试：坐标 (x, y)（窗口客户区 = surface 坐标）落在哪个行矩形上。
/// 与候选窗 `hit_test` 同语义；分隔线行同样可命中（是否消费由调用方决定）。
pub fn menu_hit_test(rows: &[Rect], x: i32, y: i32) -> Option<usize> {
    crate::layout::hit_test(rows, x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_semantics() {
        assert!(MenuEntry::separator().is_separator());
        assert!(!MenuEntry::new("设置", 1).is_separator());
        assert_eq!(MenuEntry::separator().id, 0);
    }
}
