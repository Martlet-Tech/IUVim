//! IPC 往返延迟基准（49 号 §4.5.2 P0 前置：**热路径往返实测，没有数据不开工**）。
//!
//! 对同一套管道原语（[`super::pipe::imp`]）测两种形态的单次往返：
//!
//! - **per-request-connect**（现路径形态，`daemon_client.rs` 一请求一连接）：
//!   每次 `CreateFileW 连接 → 写 → 读 → 断开`；
//! - **persistent**（M10 目标形态）：连接一次，N 次 `写 → 读` 复用同一条管道。
//!
//! 两数之差 = 长连接改造的纯收益上界；绝对值 = 49 §4.5.2 预算表 P50/P99 定档依据。
//! 在**目标真机**上跑（打字机与开发机负载不同），接入点：`cargo test -p iuv-win
//! rtt_bench -- --nocapture` 或后续 repl `--rtt-bench`。
//!
//! 纯 echo 载荷，不经过 daemon 编码/引擎——量的是**传输层**往返，即协议的下界。

use std::io;
use std::thread::JoinHandle;
use std::time::Instant;

use windows::Win32::Foundation::CloseHandle;

use super::pipe::imp;

/// 一组往返延迟统计（微秒）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RttStats {
    /// 有效样本数（= 请求次数；失败即整体报错，不产生部分样本）。
    pub n: u32,
    pub p50_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub mean_us: u64,
}

/// 基准报告：两种连接形态各一组统计。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RttReport {
    /// per-request-connect（现路径形态）往返。
    pub per_request_connect: RttStats,
    /// 长连接复用（M10 目标形态）往返。
    pub persistent: RttStats,
    /// 长连接形态的**一次**连接耗时（微秒）——per-request 形态每请求都在付这笔钱。
    pub connect_us: u64,
    /// 每次 echo 的载荷字节数。
    pub payload_len: usize,
}

impl RttReport {
    /// 单行人读摘要（日志/终端）。
    pub fn summary(&self) -> String {
        format!(
            "ipc_rtt bench: payload={}B | per-request-connect P50={}us P99={}us max={}us mean={}us | \
             persistent P50={}us P99={}us max={}us mean={}us | connect={}us",
            self.payload_len,
            self.per_request_connect.p50_us,
            self.per_request_connect.p99_us,
            self.per_request_connect.max_us,
            self.per_request_connect.mean_us,
            self.persistent.p50_us,
            self.persistent.p99_us,
            self.persistent.max_us,
            self.persistent.mean_us,
            self.connect_us,
        )
    }
}

fn stats(mut samples: Vec<u64>) -> RttStats {
    let n = samples.len() as u32;
    debug_assert!(n > 0);
    samples.sort_unstable();
    let pick = |q: u32| samples[((q as usize) * (samples.len() - 1)) / 100];
    let sum: u64 = samples.iter().sum();
    RttStats {
        n,
        p50_us: pick(50),
        p99_us: pick(99),
        max_us: samples[samples.len() - 1],
        mean_us: sum / samples.len() as u64,
    }
}

/// 跑一轮基准：`iterations` 次 echo、每次 `payload_len` 字节载荷（另加 20 次预热，
/// 不计样本）。`pipe_name` 必须是独占的测试管道名（`\\.\pipe\…`），不得与 daemon
/// 在役管道重名。
pub fn bench(pipe_name: &str, iterations: u32, payload_len: usize) -> io::Result<RttReport> {
    assert!(iterations > 0, "至少 1 次迭代");
    assert!(payload_len > 0 && payload_len <= super::pipe::PIPE_FRAME_MAX - 4);
    const WARMUP: u32 = 20;
    let payload = vec![0xABu8; payload_len];
    let name_a = format!("{pipe_name}.perreq");
    let name_b = format!("{pipe_name}.persist");

    // —— 形态一：一请求一连接（现路径形态）——
    // 服务端线程：accept → echo → 断开 ×N。客户端 drop 句柄 = 断开。
    let sa = name_a.clone();
    let server_a: JoinHandle<io::Result<()>> = std::thread::spawn(move || {
        for _ in 0..WARMUP + iterations {
            let h = imp::create_server(&imp::name_wide(&sa))?;
            imp::connect_server(h)?;
            let req = imp::read_frame(h)?;
            imp::write_frame(h, &req)?;
            // SAFETY: 服务端独占句柄；echo 完成即断开，配合客户端的用后即弃。
            unsafe {
                let _ = CloseHandle(h);
            }
        }
        Ok(())
    });
    let mut per_req: Vec<u64> = Vec::with_capacity(iterations as usize);
    for i in 0..WARMUP + iterations {
        let t = Instant::now();
        let h = connect_client_retry(&name_a)?;
        imp::write_frame(h, &payload)?;
        let echo = imp::read_frame(h)?;
        // SAFETY: 客户端独占句柄，用后即弃（与现路径 daemon_client 同形态）。
        unsafe {
            let _ = CloseHandle(h);
        }
        if echo != payload {
            return Err(io::Error::other(format!("echo 校验失败 @#{i}")));
        }
        if i >= WARMUP {
            per_req.push(t.elapsed().as_micros() as u64);
        }
    }
    server_a
        .join()
        .map_err(|e| io::Error::other(format!("服务端线程崩溃: {e:?}")))??;

    // —— 形态二：长连接复用（M10 目标形态）——
    // 服务端线程：accept 一次 → echo ×N → 客户端断开使 read 失败退出。
    let sb = name_b.clone();
    let server_b: JoinHandle<io::Result<()>> = std::thread::spawn(move || {
        let h = imp::create_server(&imp::name_wide(&sb))?;
        imp::connect_server(h)?;
        // 客户端断开使 read 失败 = 正常结束。
        while let Ok(req) = imp::read_frame(h) {
            imp::write_frame(h, &req)?;
        }
        // SAFETY: 服务端独占句柄；客户端已断开。
        unsafe {
            let _ = CloseHandle(h);
        }
        Ok(())
    });
    let t_connect = Instant::now();
    let h = connect_client_retry(&name_b)?;
    let connect_us = t_connect.elapsed().as_micros() as u64;
    let mut persist: Vec<u64> = Vec::with_capacity(iterations as usize);
    for i in 0..WARMUP + iterations {
        let t = Instant::now();
        imp::write_frame(h, &payload)?;
        let echo = imp::read_frame(h)?;
        if echo != payload {
            return Err(io::Error::other(format!("echo 校验失败 @#{i}")));
        }
        if i >= WARMUP {
            persist.push(t.elapsed().as_micros() as u64);
        }
    }
    // SAFETY: 客户端独占句柄；关闭使服务端 read_frame 失败退出。
    unsafe {
        let _ = CloseHandle(h);
    }
    server_b
        .join()
        .map_err(|e| io::Error::other(format!("服务端线程崩溃: {e:?}")))??;

    Ok(RttReport {
        per_request_connect: stats(per_req),
        persistent: stats(persist),
        connect_us,
        payload_len,
    })
}

/// 测试管道名辅助：进程内唯一的 `\\.\pipe\iuv-rtt-<pid>-<tag>`。
pub fn test_pipe_name(tag: &str) -> String {
    format!(r"\\.\pipe\iuv-rtt-{}-{tag}", std::process::id())
}

/// 客户端连接（含 NOT_FOUND 短重试）：服务端两次 accept 之间/首个实例创建前存在
/// 微秒级「管道不存在」窗口（ERROR_FILE_NOT_FOUND），与 daemon 在线判定无关——
/// 基准里重试到管道出现为止（上界 2s，超时按真错误上报）。
fn connect_client_retry(name: &str) -> io::Result<windows::Win32::Foundation::HANDLE> {
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match imp::connect_client(&imp::name_wide(name)) {
            Ok(h) => return Ok(h),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if Instant::now() > deadline {
                    return Err(e);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => return Err(e),
        }
    }
}
