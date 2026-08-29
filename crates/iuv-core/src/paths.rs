//! 用户数据目录解析（TSF / daemon / 配置加载三处共用的唯一实现）。
//!
//! 回退链：`%LOCALAPPDATA%` → `%APPDATA%\Local` → `%USERPROFILE%\AppData\Local` → `%HOME%`。
//! 提升权限或精简环境的进程可能缺前两级，逐级兜底保证同一目录语义；
//! 全部未设（非 Windows 开发环境）→ None。

use std::path::PathBuf;

/// 用户本地数据根目录（词库/用户库/config.json 的父目录链）。
pub fn local_data_dir() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA")
        .ok()
        .or_else(|| std::env::var("APPDATA").ok().map(|a| format!("{a}\\Local")))
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .map(|p| format!("{p}\\AppData\\Local"))
        })
        .or_else(|| std::env::var("HOME").ok())
        .map(PathBuf::from)
}

/// iuv 数据目录（`<local_data_dir>\iuv`）。词库/用户库/config.json 同目录（契约 §7）。
pub fn iuv_dir() -> Option<PathBuf> {
    local_data_dir().map(|p| p.join("iuv"))
}
