//! 版本资源（winres）。契约 13 任务书 §3.7。
//! 【Agent D】W1 实现。
//!
//! 图标资源：
//! - `icon.ico` → ID "1"：DLL/输入法图标（应用图标）
//! - `zh.ico`   → ID "101"：语言栏"中"图标（LoadImageW + MAKEINTRESOURCE(101)）
//! - `en.ico`   → ID "102"：语言栏"英"图标（LoadImageW + MAKEINTRESOURCE(102)）
//!
//! ID 对齐 Weasel（IDI_ZH=101/IDI_EN=102 同语义），契约 01 §5.1。

fn main() {
    // 资源文件变化时重跑本脚本。
    println!("cargo:rerun-if-changed=res/icon.ico");
    println!("cargo:rerun-if-changed=res/zh.ico");
    println!("cargo:rerun-if-changed=res/en.ico");

    // 逃生舱（仅显式设置时生效，默认行为不变）：沙箱/CI 里 `reg.exe` 被安全策略
    // 拦截时 winres 查不到 Windows SDK 路径 → rc.exe 解析失败 → 本脚本 panic，
    // 连带整个 `cargo check -p iuv-tsf` 无法做类型检查。置此变量即跳过资源编译，
    // 只做类型/借用检查（产物无版本资源，**不可用于发布/装载**）。
    if std::env::var_os("IUV_SKIP_WINRES").is_some() {
        println!(
            "cargo:warning=IUV_SKIP_WINRES 已置位：跳过版本资源编译（仅类型检查用，产物不可装载）"
        );
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set_icon("res/icon.ico")
        .set_icon_with_id("res/zh.ico", "101")
        .set_icon_with_id("res/en.ico", "102");
    res.set("FileDescription", "IUV 输入法 - 中文输入法（TSF 文本服务）");
    res.set("ProductName", "IUV 输入法");
    res.set("FileVersion", "0.1.0.0");
    res.set("ProductVersion", "0.1.0.0");
    res.set("OriginalFilename", "iuv_tsf.dll");
    res.set("InternalName", "iuv_tsf.dll");
    res.set("LegalCopyright", "MIT License");
    if let Err(e) = res.compile() {
        // build.rs 中 panic 会中止构建并显示错误信息。
        panic!("winres 资源编译失败：{e}");
    }
}
