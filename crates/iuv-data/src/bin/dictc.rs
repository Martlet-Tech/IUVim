//! dictc：词库编译 CLI。契约 01-contract.md §3 与任务书 10 §3.3。
//! 用法：
//!   dictc <output.imedic> <input1.dict.yaml> [input2...]      # 拼音词库 → IMEDIC02
//!   dictc opencc <output.opencc> <STPhrases.txt> <STCharacters.txt>  # OpenCC 简繁表 → IUVOCC01

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "用法: dictc <output.imedic> <input1.dict.yaml> [input2...] | dictc opencc <output.opencc> <STPhrases.txt> <STCharacters.txt>"
        );
        std::process::exit(2);
    }
    if args[0] == "opencc" {
        run_opencc(&args);
        return;
    }
    if args.len() < 2 {
        eprintln!("用法: dictc <output.imedic> <input1.dict.yaml> [input2...]");
        std::process::exit(2);
    }
    let output = &args[0];
    let inputs: Vec<std::path::PathBuf> = args[1..].iter().map(std::path::PathBuf::from).collect();
    match iuv_data::compile_files(&inputs, std::path::Path::new(output)) {
        Ok(stats) => {
            println!(
                "files={} entries={} codes={} duplicates={}",
                stats.files, stats.entries, stats.codes, stats.duplicates
            );
        }
        Err(e) => {
            eprintln!("编译失败: {e}");
            std::process::exit(1);
        }
    }
}

/// dictc opencc 子命令：OpenCC 两个文本 → IUVOCC01 二进制。
fn run_opencc(args: &[String]) {
    if args.len() != 4 {
        eprintln!("用法: dictc opencc <output.opencc> <STPhrases.txt> <STCharacters.txt>");
        std::process::exit(2);
    }
    let output = std::path::Path::new(&args[1]);
    let phrases = std::path::Path::new(&args[2]);
    let chars = std::path::Path::new(&args[3]);
    match iuv_data::opencc::compile_files(phrases, chars, output) {
        Ok(entries) => println!("opencc entries={} -> {}", entries, output.display()),
        Err(e) => {
            eprintln!("OpenCC 编译失败: {e}");
            std::process::exit(1);
        }
    }
}