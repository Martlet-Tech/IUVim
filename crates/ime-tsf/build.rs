//! 版本资源（winres）。契约 13 任务书 §3.7。
//! 【Agent D】W1 实现。
//!
//! 图标资源：
//! - `icon.ico` → ID "1"：DLL/输入法图标（应用图标）
//! - `zh.ico`   → ID "101"：语言栏"中"图标（LoadImageW + MAKEINTRESOURCE(101)）
//! - `en.ico`   → ID "102"：语言栏"英"图标（LoadImageW + MAKEINTRESOURCE(102)）
//! ID 对齐 Weasel（IDI_ZH=101/IDI_EN=102 同语义），契约 01 §5.1。

fn main() {
    // 资源文件变化时重跑本脚本。
    println!("cargo:rerun-if-changed=res/icon.ico");
    println!("cargo:rerun-if-changed=res/zh.ico");
    println!("cargo:rerun-if-changed=res/en.ico");

    let mut res = winres::WindowsResource::new();
    res.set_icon("res/icon.ico")
        .set_icon_with_id("res/zh.ico", "101")
        .set_icon_with_id("res/en.ico", "102");
    res.set("FileDescription", "Input IME - 中文输入法（TSF 文本服务）");
    res.set("ProductName", "Input IME");
    res.set("FileVersion", "0.1.0.0");
    res.set("ProductVersion", "0.1.0.0");
    res.set("OriginalFilename", "input_ime_tsf.dll");
    res.set("InternalName", "input_ime_tsf.dll");
    res.set("LegalCopyright", "MIT License");
    if let Err(e) = res.compile() {
        // build.rs 中 panic 会中止构建并显示错误信息。
        panic!("winres 资源编译失败：{e}");
    }
}
