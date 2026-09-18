//! 46 号任务书 §5 基线（B0）与验收工具。
//!
//! 单一入口、输出可分股：
//! - **内容行 → stdout**（`SEG/CANDS/READING/COMPOSITION/PAGE`），逐字节可 diff；
//! - **计时行 → stderr**（`[time]` 前缀），非确定性，单独重定向。
//!
//! 用法（索引 iuvim 仓库根）：
//! ```text
//! cargo test -p iuv-core --test corpus_baseline -- --ignored --nocapture `
//!     > "$env:TEMP\iuv-46\b0.txt" 2> "$env:TEMP\iuv-46\b0-timings.txt"
//! ```
//! B0 = 46 号改造**前**（`main`）现语义；改造后重跑同一命令，内容行除
//! §6.4 专项差异用例（>128 切分方案且整串成词 → 整跨词优先）外应零漂移。

use iuv_core::api::{EngineCtx, ImeEngine, PendingInput};
use iuv_core::{Config, Engine, Key, RimeEngine};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 语料 = 39 号 §15A 十二条 + 46 号长串哨兵 + 切分决策敏感边界用例。
///
/// 后四类短输入专门钉住 `best_seg` 反查必须等价复现的旧 `rank_plans` 语义：
/// 词频重排（`fenge`/`dier`/`keneng`）、大写保形（`niHAO`）、
/// 原文兜底（`input`/`qaz`）、强制撇号与空段（`xi'an`/`x'`）、üe 归一（`lue`/`nue`）。
const CORPUS: &[&str] = &[
    // —— 39 号 §15A λ 校准基线语料（12 条）——
    "nihao",
    "xian",
    "shigechengy",
    "haoshengy",
    "nhmsx",
    "nhao",
    "nihaoshijie",
    "sh",
    "zheshiming",
    "chuangqianmingyueguang",
    "zhongguorenmin",
    "xiexiedajia",
    // —— 46 号长串哨兵（尾型含独立元音 + 简拼孤点，旧 seg 指数爆点）——
    "beiguofengguangqianlibingfengwanlixuepiaowangchang",
    "ceshiyixiaxianzaiyongwozhegeshurufadouchunbuganjuezianzaihaoxiang",
    "ceshiyixiaxianzaiyongwozhegeshurufadachuhenduoshurushihouhaibukabuguoganjuexianzaihaoxiang",
    // —— 切分决策（词频重排）敏感用例 ——
    "fenge",
    "dier",
    "keneng",
    // —— 大写保形 / 原文兜底 / 撇号空段 / üe 归一 ——
    "niHAO",
    "input",
    "qaz",
    "xi'an",
    "x'",
    "lue",
    "nue",
    "gonglue",
];

/// 真词库路径（`CARGO_MANIFEST_DIR` = crates/iuv-core，仓库根为 ../../）。
fn dict_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/iuv.imedic"
    ))
}

/// 引擎内部埋点收集槽（`iuv_core::perf` 的 sink 只能是 `fn` 指针，故走静态槽）。
/// 仅在验收工具里开启，用于把 `onkey` 拆成 seg/graph/buckets/assemble 四段。
static PHASES: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());

fn perf_sink(phase: &'static str, micros: u64) {
    if let Ok(mut v) = PHASES.lock() {
        v.push((phase, micros));
    }
}

fn drain_phases() -> Vec<(&'static str, u64)> {
    PHASES
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

/// 打印某语料阶段分解（每阶段一行，避免宿主机 127 字符长行截断）。
fn dump_phases(idx: usize, phases: &[(&'static str, u64)]) {
    let mut names: Vec<&'static str> = phases.iter().map(|(n, _)| *n).collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        let sel: Vec<u64> = phases
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, us)| *us)
            .collect();
        eprintln!(
            "[time] PH i={idx} {name} n={} sum_us={} max_us={}",
            sel.len(),
            sel.iter().sum::<u64>(),
            sel.iter().copied().max().unwrap_or(0)
        );
    }
}

#[test]
#[ignore = "需真词库 data/iuv.imedic（索引 iuvim 仓库根运行）"]
fn corpus_baseline_dump() {
    let dict = Arc::new(
        iuv_data::load(&dict_path())
            .unwrap_or_else(|e| panic!("词库加载失败: {}: {e}", dict_path().display())),
    );
    let cfg = Config::default();
    // 引擎与词库共享同一 mmap（Dict::clone 共享 Arc<MappedFile>）。
    let engine = RimeEngine::new(dict.clone(), &cfg);
    // 阶段埋点（`perf_probe` 机制原样复用，不改其实现；仅本验收工具开启）。
    iuv_core::perf::set_sink(perf_sink);
    iuv_core::perf::set_enabled(true);

    for (idx, &raw) in CORPUS.iter().enumerate() {
        drain_phases();
        // ---- ① 引擎层：分段视图 + 候选（translate 输出）----
        let t0 = Instant::now();
        let tr = engine.translate(&EngineCtx { preceding_text: "" }, &PendingInput { raw });
        let whole_us = t0.elapsed().as_micros();
        let seg = tr
            .segmentation
            .first()
            .map(|s| s.syllables.join("'"))
            .unwrap_or_default();
        println!("== RAW {raw}");
        println!("SEG {seg}");
        println!("CANDS {}", render(&tr.candidates, 9));

        // ---- ② 会话层：预编辑/上屏视图（覆盖 preedit 与部分消费口径）----
        let mut s = Engine::new((*dict).clone(), cfg.clone()).start_session();
        for ch in raw.chars() {
            // 与 TSF `session_bridge` 同规：大写走 ShiftChar（保形进序列），
            // `Key::Char` 只收小写与撇号，喂错变体会让大写键被丢弃、会话失真。
            s.on_key(if ch.is_ascii_uppercase() {
                Key::ShiftChar(ch)
            } else {
                Key::Char(ch)
            });
        }
        let eff = s.effect();
        println!("READING {}", eff.reading);
        println!("COMPOSITION {}", eff.composition);
        println!("PAGE {}", render(&eff.candidates, usize::MAX));
        eprintln!("[time] WHOLE raw={raw} us={whole_us}");

        // ---- ③ 长串哨兵：逐字节前缀计时（与真实打字一致，逐字节非音节边界）----
        if raw.len() >= 20 {
            debug_assert!(raw.is_ascii(), "哨兵须为纯 ASCII 拼音串");
            let mut total = 0u128;
            let mut max = 0u128;
            let mut max_at = 0usize;
            for i in 1..=raw.len() {
                let pre = &raw[..i];
                let t = Instant::now();
                let _ = engine.translate(
                    &EngineCtx { preceding_text: "" },
                    &PendingInput { raw: pre },
                );
                let us = t.elapsed().as_micros();
                total += us;
                if us > max {
                    max = us;
                    max_at = i;
                }
                eprintln!("[time] PERKEY {i} {us}");
            }
            // 注意：Windows 宿主的 stderr 重定向会截断超长行（实测 127 字符），
            // 故 raw 与数值分两行打印（`PERKEYSUM` 行恒短，数值一定完整）。
            eprintln!("[time] RAWSUM {raw}");
            eprintln!(
                "[time] PERKEYSUM keys={} total_us={total} max_us={max} max_at={max_at}",
                raw.len()
            );
        }

        // ---- ④ 阶段分解（seg/graph/buckets/assemble；引擎内部埋点转发）----
        dump_phases(idx, &drain_phases());
    }
}

/// `候选文本:消费段数` 列表（取前 `limit` 条；`usize::MAX` = 全量）。
/// 有意不含 `weight`/`score` 浮点，避免任何格式化不确定性。
fn render(cands: &[iuv_core::Candidate], limit: usize) -> String {
    cands
        .iter()
        .take(limit)
        .map(|c| format!("{}:{}", c.text, c.seg_len))
        .collect::<Vec<_>>()
        .join("|")
}
