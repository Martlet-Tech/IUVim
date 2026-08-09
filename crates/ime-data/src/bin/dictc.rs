//! dictc：词库编译 CLI。契约 01-contract.md §3 与任务书 10 §3.3。
//! 【Agent A】W1 实现。
//! 用法：`dictc <output.imedic> <input1.dict.yaml> [input2...]`

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("用法: dictc <output.imedic> <input1.dict.yaml> [input2...]");
        std::process::exit(2);
    }
    let output = &args[0];
    let inputs: Vec<std::path::PathBuf> = args[1..].iter().map(std::path::PathBuf::from).collect();
    match ime_data::compile_files(&inputs, std::path::Path::new(output)) {
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
