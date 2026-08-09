//! 版本资源（winres）。契约 13 任务书 §3.7。
//! 【Agent D】W1 实现。

fn main() {
    // 资源文件变化时重跑本脚本。
    println!("cargo:rerun-if-changed=res/icon.ico");

    let mut res = winres::WindowsResource::new();
    res.set_icon("res/icon.ico");
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
