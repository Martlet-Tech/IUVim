//! 用户数据存储。契约 01-contract.md §4 store.rs。
//! 【Agent B】W1 实现。W0 仅空壳。

use std::time::SystemTime;

/// 用户数据存储。MVP 用 NullStore。
pub trait UserDataStore: Send {
    fn record_selection(&mut self, code: &str, text: &str, now: SystemTime);
    /// M2 滞回模型用（有效使用强度）；MVP 恒返回 0.0。
    fn power(&self, code: &str, text: &str, now: SystemTime) -> f32;
    /// 持久化钩子，MVP 空实现
    fn flush(&mut self) {}
}

/// 全空实现。
pub struct NullStore;

impl UserDataStore for NullStore {
    fn record_selection(&mut self, _code: &str, _text: &str, _now: SystemTime) {}
    fn power(&self, _code: &str, _text: &str, _now: SystemTime) -> f32 {
        0.0
    }
}
