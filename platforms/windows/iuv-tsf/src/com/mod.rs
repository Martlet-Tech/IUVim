//! COM 基础设施：class factory / text service。契约 13 任务书 §3.2-3.3。
//! 【Agent D】W1 实现。

pub mod class_factory;
pub(crate) mod daemon_host;
pub(crate) mod dispatch;
pub(crate) mod engine_host;
pub(crate) mod key_routing;
pub(crate) mod mode;
pub mod text_service;
