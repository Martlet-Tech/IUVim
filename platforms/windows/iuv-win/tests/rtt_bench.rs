//! IPC 往返延迟基准（49 号 §4.5.2 P0 前置）。
//!
//! 跑法：`cargo test -p iuv-win --test rtt_bench -- --nocapture`
//! 真机数据贴回 49 号 §4.5.2 预算表。纯 echo，不依赖 daemon 在线。

use iuv_win::ipc::{rtt_bench, rtt_test_pipe_name};

#[test]
fn rtt_bench_reports_both_connection_shapes() {
    let report = rtt_bench(&rtt_test_pipe_name("bench"), 500, 64)
        .expect("基准应成功（echo 校验 + 无 IO 错误）");
    // --nocapture 下打印，供真机数据采集。
    println!("{}", report.summary());

    let per = report.per_request_connect;
    let per_con = report.persistent;
    assert_eq!(per.n, 500);
    assert_eq!(per_con.n, 500);
    // 合理性护栏（防 flaky 的宽松上界）：管道 echo 正常应在百微秒级，10ms 已是异常。
    assert!(
        per.p50_us < 10_000,
        "per-request P50 异常: {}us",
        per.p50_us
    );
    assert!(
        per_con.p50_us < 10_000,
        "persistent P50 异常: {}us",
        per_con.p50_us
    );
    // 每个样本都为正（时钟分辨率内至少 1us）。
    assert!(per.p50_us >= 1 && per_con.p50_us >= 1);
}
