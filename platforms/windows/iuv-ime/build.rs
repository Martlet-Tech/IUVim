//! 构建脚本：嵌入 VS_VERSION_INFO 版本资源（IMM IME 加载要求，参照 weasel.ime）。

fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_version_info(winres::VersionInfo::FILEVERSION, 0x0001_0000_0000_0000);
    res.set_version_info(winres::VersionInfo::PRODUCTVERSION, 0x0001_0000_0000_0000);
    res.set("CompanyName", "iuv");
    res.set("ProductName", "iuv");
    res.set("FileDescription", "iuv input method IMM component");
    res.set("OriginalFilename", "iuv.ime");
    res.set("InternalName", "iuv.ime");
    if let Err(e) = res.compile() {
        println!("cargo:warning=winres 编译失败: {e}");
    }
}
