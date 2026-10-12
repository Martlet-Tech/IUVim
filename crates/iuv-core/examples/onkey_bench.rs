//! on_key 端到端基准：用真实词库直接驱动 `Session`，**排除 IPC / 序列化 / 候选窗**
//! 因素，测纯引擎的每键耗时。
//!
//! 运行：
//!   CARGO_TARGET_DIR=target-server cargo run --release -p iuv-core \
//!       --example onkey_bench -- [dict_path] [rounds]
//!
//! 默认词库：%LOCALAPPDATA%\iuv\iuv.imedic（与部署一致）。
//! 与线上 [perf] 对账：线上 on_key = 本基准的 session.on_key + outcome() + respond() +
//! sync_candwin()。若本基准明显更快，说明开销在服务端后半段（IPC/候选窗/克隆）。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iuv_core::{Config, Engine, Key};

fn arg_or(idx: usize, default: &str) -> String {
    std::env::args()
        .nth(idx)
        .unwrap_or_else(|| default.to_string())
}

fn dict_path() -> PathBuf {
    if let Some(p) = std::env::args().nth(1) {
        return PathBuf::from(p);
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("iuv").join("iuv.imedic")
}

struct Stat {
    n: usize,
    total: Duration,
    max: Duration,
    max_at: usize,
}

fn feed(
    engine: &std::sync::Arc<Engine>,
    case: &str,
) -> (Stat, Vec<(usize, char, Duration, usize)>) {
    let mut session = engine.start_session();
    let mut per_key = Vec::new();
    let mut stat = Stat {
        n: 0,
        total: Duration::ZERO,
        max: Duration::ZERO,
        max_at: 0,
    };
    for (i, ch) in case.chars().enumerate() {
        let t = Instant::now();
        let eff = session.on_key(Key::Char(ch));
        let dt = t.elapsed();
        stat.n += 1;
        stat.total += dt;
        if dt > stat.max {
            stat.max = dt;
            stat.max_at = i;
        }
        per_key.push((i, ch, dt, eff.candidates.len()));
    }
    (stat, per_key)
}

fn main() {
    let path = dict_path();
    let rounds: usize = arg_or(2, "3").parse().unwrap_or(3);

    println!("== iuv on_key 基准 ==");
    println!("词库：{}", path.display());
    let t0 = Instant::now();
    let dict = match iuv_data::load(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("词库加载失败：{e}");
            std::process::exit(1);
        }
    };
    println!("词条数：{}（加载 {:?}）", dict.entry_count(), t0.elapsed());
    let cfg = Config::load();
    println!(
        "配置：page_size={} candidate_prefix={}",
        cfg.page_size, cfg.candidate_prefix
    );
    let engine = Engine::new(dict, cfg);
    // 真实 server（main.rs::load_engine）会挂用户词库——`Dict::merged` 的
    // 屏蔽/调权/独有条目分支随之激活，是线上比裸基准慢的关键变量。此处对齐。
    let user_path = path
        .parent()
        .map(|d| d.join("iuv.user.imedic"))
        .unwrap_or_else(|| PathBuf::from("iuv.user.imedic"));
    match engine.attach_user_dict(user_path.clone()) {
        Ok(()) => println!("用户词库：{}", user_path.display()),
        Err(e) => println!("用户词库装配失败（空库继续）：{e}"),
    }

    let cases: &[&str] = &[
        "ni",                      // 单音节
        "nihao",                   // 双音节
        "jintiantianqizhenbucuo",  // 长句（4+ 音节）
        "zhonghuarenmingongheguo", // 长词
        "woshiyigechengxuyuan",    // 常用句
        "xianzaiyoumeiyoukaqi",    // 更长
        "wangshangliulanqi",       // 词组
        "chifanhema",              // 短句
    ];

    // 预热（首键含会话初始化、词典热路径预取）
    let _ = feed(&engine, "nihaoshijie");
    let _ = feed(&engine, "jintiantianqi");

    println!(
        "\n{:<26} {:>8} {:>10} {:>10} {:>10}",
        "输入串", "键数", "总耗时", "均值", "最大"
    );
    println!("{}", "-".repeat(70));
    for case in cases {
        let mut agg = Stat {
            n: 0,
            total: Duration::ZERO,
            max: Duration::ZERO,
            max_at: 0,
        };
        let mut worst: Vec<(usize, char, Duration, usize)> = Vec::new();
        for _ in 0..rounds {
            let (s, pk) = feed(&engine, case);
            agg.n += s.n;
            agg.total += s.total;
            if s.max > agg.max {
                agg.max = s.max;
                agg.max_at = s.max_at;
            }
            for row in pk {
                worst.push(row);
            }
        }
        worst.sort_by_key(|b| std::cmp::Reverse(b.2));
        let avg = agg.total / agg.n.max(1) as u32;
        println!(
            "{:<26} {:>8} {:>10.1?} {:>10.2?} {:>10.2?}",
            case,
            case.chars().count(),
            agg.total / rounds as u32,
            avg,
            agg.max
        );
        // 打印该串最慢的 3 个键（含位置/字符/候选数）
        let mut shown = Vec::new();
        for (i, ch, dt, nc) in worst.iter().take(30) {
            if dt.as_millis() < 10 {
                break;
            }
            let tag = format!("{}@{}({})", ch, i, nc);
            if shown.contains(&tag) {
                continue;
            }
            shown.push(tag);
            if shown.len() >= 3 {
                break;
            }
        }
        if !shown.is_empty() {
            println!("    ↳ 慢键：{}", shown.join("  "));
        }
    }
}
