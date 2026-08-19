//! 简→繁转换器薄壳（31-script-traditional.md）。
//!
//! 数据 = IUVOCC01 二进制（iuv-data::OpenccTable，OpenCC s2t 通用繁体）；本模块只在
//! iuv-core 侧提供 `ScriptConverter`（Arc 包装 + 是否装配的判定），转换算法在 iuv-data
//! （`OpenccTable::convert`，正向最长匹配）。Engine 经 `attach_script_converter` 装配，
//! Session 在 `to_output`/`effect` 挂转换（仅 `initial_state.script == Traditional` 时）。

use iuv_data::OpenccTable;
use std::sync::Arc;

/// 简→繁转换器（只读共享，跨线程安全）。
#[derive(Clone, Debug, Default)]
pub struct ScriptConverter {
    table: Arc<OpenccTable>,
}

impl ScriptConverter {
    /// 由已解析的转换表构建。
    pub fn new(table: OpenccTable) -> ScriptConverter {
        ScriptConverter {
            table: Arc::new(table),
        }
    }

    /// 空转换器（未装配/降级；convert 返回原文）。
    pub fn empty() -> ScriptConverter {
        ScriptConverter::default()
    }

    /// 简→繁转换：委托 `OpenccTable::convert`（正向最长匹配，短语优先 + 单字兜底）。
    pub fn convert(&self, text: &str) -> String {
        self.table.convert(text)
    }

    /// 词条数（日志用）。
    pub fn entry_count(&self) -> usize {
        self.table.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_data::opencc::from_text;

    #[test]
    fn converter_delegates_convert() {
        let table = from_text("以后\t以後\n", "").unwrap();
        let c = ScriptConverter::new(table);
        assert_eq!(c.convert("以后"), "以後");
        assert_eq!(c.convert("nihao"), "nihao");
        assert_eq!(c.entry_count(), 1);
    }

    #[test]
    fn empty_converter_passthrough() {
        let c = ScriptConverter::empty();
        assert_eq!(c.convert("以后"), "以后");
    }
}