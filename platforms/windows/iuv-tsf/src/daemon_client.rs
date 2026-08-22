//! M6 daemon 客户端（会话进程侧）：共享段只读映射 + 命名管道写请求。
//! 设计见 `docs/plan/22-m6-daemon.md` §3 与「会话进程客户端对接规格」。
//!
//! 会话进程对用户库"引用" = 只读共享内存段（`ShmReader`）+ 版本检测重载：
//! 查询仍在本地（不做 IPC 查询代理），写请求（调权/自造/隐藏）走命名管道（`PipeClient`）。
//! daemon 不在线 → 降级现状（引擎侧本地写盘；`UserRemote::apply` 返回 false 即天然兜底），
//! 绝不挂键/拖慢按键（poll 成本 = 读一个 u32 版本）。
//!
//! 线程：DaemonClient 全程在 TSF 线程使用（与 Engine 同线程，STA）。内部字段全部
//! `Mutex` 包裹仅为满足 `UserRemote::apply(&self)` 签名（引擎经 `Arc<dyn UserRemote>`
//! 调用，只有 `&self`）——实际无跨线程竞争（见 unsafe impl 的 SAFETY 注释）。
//!
//! 全部 IO 失败静默降级（记日志），不 panic（DLL 内硬性约定）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use iuv_core::{Engine, ImeState, UserMutation, UserRemote};
use iuv_win::{
    PipeClient, Request, Response, ShmReader, SignalClient, ToolbarSignal,
};
use windows::Win32::System::Threading::{
    CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::log::log_line;

/// daemon 自启节流（秒）：Activate 检测离线后 60s 内不重复拉起（防多进程/多键风暴；
/// 并发拉起由 daemon 单实例互斥兜底）。
const LAUNCH_COOLDOWN_SECS: u64 = 60;

/// M6 daemon 客户端。单实例经 `Arc` 与引擎 `user_remote` 共享（text_service 持一份、
/// 引擎持一份），全部方法 `&self`（内部 Mutex）。
pub struct DaemonClient {
    /// 命名管道连接（惰性：首次 `request_once`（send_request/pipe_online）时 connect）。
    /// **每笔请求后用即弃**——服务端一连接只服务一请求（accept → serve → DisconnectNamedPipe，
    /// 刻意的一请求一连接以公平服务多进程客户端），缓存复用会把下笔请求打到已断开的句柄
    /// （实测 0x800700E9 ERROR_PIPE_BROKEN，靠重连自愈但多一次失败往返）。
    /// 失败/断开 → 清缓存，下次新建。
    pipe: Mutex<Option<PipeClient>>,
    /// 工具条信号通道连接（惰性：首次 `send_signal` 时 connect；写失败弃缓存，
    /// 下次发送前重连——持久连接的自然生命周期，非兜底机制）。
    /// 与数据面 `pipe` 物理隔离：控制面零争用，焦点风暴下消息不丢。
    signal: Mutex<Option<SignalClient>>,
    /// 共享段只读映射（惰性打开：daemon 未建段 → None = 离线信号）。
    shm: Mutex<Option<ShmReader>>,
    /// 已消费的用户库版本（version 变化 → 重解析段注入引擎）。
    last_version: Mutex<u32>,
    /// 已消费的配置纪元（config_epoch 变化 → 回调重载 config）。
    last_epoch: Mutex<u32>,
    /// 用户库文件路径（离线翻转日志用：降级写盘目标提示）。
    user_path: PathBuf,
    /// daemon 在线状态（离线→在线 / 在线→离线翻转日志）。
    online: Mutex<bool>,
}

// SAFETY: DaemonClient 全程在 TSF 线程使用（与 Engine 同线程，STA；COM 单线程租约）。
// 内部 Mutex 只用于 UserRemote::apply 的 `&self` 签名（无跨线程竞争——与 shm.rs 的
// ShmWriter/ShmReader 同理由）；PipeClient 持 HANDLE（raw pointer，!Send），经本
// unsafe impl 声明"仅单线程使用 + Mutex 串行访问"：句柄值随 Arc 移动无碍（kernel 句柄
// 值本身线程无关），真正使用总在 Mutex 内。CloseHandle 跨线程安全（Drop 可发生在任意
// 线程）。
unsafe impl Send for DaemonClient {}
unsafe impl Sync for DaemonClient {}

impl DaemonClient {
    /// 构造。`user_path` = 现有 iuv.user.imedic 路径（离线日志用）。
    pub fn new(user_path: PathBuf) -> Self {
        DaemonClient {
            pipe: Mutex::new(None),
            signal: Mutex::new(None),
            shm: Mutex::new(None),
            last_version: Mutex::new(0),
            last_epoch: Mutex::new(0),
            user_path,
            online: Mutex::new(false),
        }
    }

    /// 守护进程自启（M7，搜狗同款 IME 惰性拉起机制）：daemon 不在线且冷却期满 →
    /// `CreateProcessW` 拉起 DLL 同目录 `iuv-daemon.exe`（后台无控制台，异步不等待）。
    /// Activate 时调用（用户切到本输入法的时机）。返回 true = daemon 已在线/已拉起；
    /// false = 离线且未拉起（冷却中/路径缺失/失败——静默降级，绝不 panic）。
    pub fn ensure_daemon(&self) -> bool {
        // 1. 在线检测：管道 Ping（daemon 存活的准确信号）。**不能**用共享段存在判定——
        //    段会被各会话进程 ShmReader 引用残留，daemon 被杀后依然 open 成功，
        //    导致 ensure_daemon 误判在线不拉起（2026-08-17 实测假死）。
        if self.pipe_online() {
            return true;
        }
        // 2. 冷却节流（进程级）：60s 内仅尝试一次。
        if !launch_cooldown_ok() {
            return false;
        }
        // 3. 路径解析：DLL 同目录 iuv-daemon.exe（安装目录 = %ProgramFiles%\iuv\，
        //    dev/test 目录同样成立）。
        let Some(exe) = daemon_exe_path() else {
            log_line("[daemon] 自启：未找到 iuv-daemon.exe（DLL 同目录），跳过");
            return false;
        };
        // 4. 拉起（异步，不等待；CREATE_NO_WINDOW 后台无控制台）。
        let mut cmd: Vec<u16> = exe
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: STARTUPINFOW/PROCESS_INFORMATION 全程存活；cmdline 可写缓冲（系统可改）。
        let mut si = STARTUPINFOW::default();
        si.cb = size_of::<STARTUPINFOW>() as u32;
        let mut pi = PROCESS_INFORMATION::default();
        let r = unsafe {
            CreateProcessW(
                None,
                Some(windows::core::PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW,
                None,
                None,
                &si,
                &mut pi,
            )
        };
        match r {
            Ok(()) => {
                // SAFETY: pi 句柄用完即关（daemon 独立生命周期，不等待不管理）。
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(pi.hProcess);
                    let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
                }
                log_line(&format!("[daemon] 已拉起守护进程：{exe:?}"));
                true
            }
            Err(e) => {
                log_line(&format!("[daemon] 自启失败：{e:?}"));
                false
            }
        }
    }

    /// 轮询共享段（text_service 每键 handle_key_down 最前部调用；低成本：读 u32 版本）。
    ///
    /// 逻辑（22-m6-daemon.md「会话进程客户端对接规格」）：
    /// 1. `ShmReader` 打开失败（daemon 未建段）→ 离线，返回 false；
    /// 2. `version != last_version` → 重解析段 → `Some(user)` 则 `engine.set_user_dict`
    ///    （只注入内存态，不写盘），更新 last_version（None/Err 跳过——段未写入/损坏）；
    /// 3. `config_epoch != last_epoch` → 调用 `on_config_epoch`（text_service 注入：
    ///    engine.set_config + candwin.set_theme 等），更新 last_epoch；
    /// 4. 在线状态翻转记日志；**离线→在线**时调用 `on_online`（text_service 注入：
    ///    重新 Register 工具栏实例——§4.4 daemon 重启恢复，实例表/看板失联重建）；
    ///    不把引擎 user_remote 置 None（apply 返回 false 即降级，天然兜底）。
    ///
    /// 返回 true = 用户库或配置有变化（调用方无需特殊处理，仅日志/断言用）。
    pub fn poll(
        &self,
        engine: &Arc<Engine>,
        on_config_epoch: impl Fn(&Arc<Engine>),
        on_online: impl Fn(),
    ) -> bool {
        // 1. 共享段打开失败 → 离线（写路径自动降级本地）。
        let (version, epoch) = {
            let mut shm = self.shm.lock().unwrap_or_else(|e| e.into_inner());
            if shm.is_none() {
                match ShmReader::open() {
                    Ok(r) => *shm = Some(r),
                    Err(e) => {
                        log_line(&format!("[daemon] 共享段打开失败（daemon 离线）：{e}"));
                        self.set_online(false);
                        return false;
                    }
                }
            }
            let reader = shm.as_ref().expect("shm 刚已确认存在");
            (reader.version(), reader.config_epoch())
        };

        let mut changed = false;

        // 2. version 变化 → 重解析段注入引擎（读到的是该 version 对应的完整数据，
        //    写序由 shm.rs 的 version(Release) 保证——无"半新半旧"）。
        if version != *self.last_version.lock().unwrap_or_else(|e| e.into_inner()) {
            let read = {
                let shm = self.shm.lock().unwrap_or_else(|e| e.into_inner());
                shm.as_ref().and_then(|r| match r.read() {
                    Ok(Some(user)) => Some(user),
                    Ok(None) => {
                        log_line("[daemon] 共享段存在但未写入（版本先于首次写）→ 跳过");
                        None
                    }
                    Err(e) => {
                        log_line(&format!("[daemon] 共享段解析失败（保持旧库）：{e}"));
                        None
                    }
                })
            };
            if let Some(user) = read {
                engine.set_user_dict(Arc::new(user));
                *self.last_version.lock().unwrap_or_else(|e| e.into_inner()) = version;
                log_line(&format!(
                    "[daemon] 用户库版本 {version}：注入引擎（共享段只读引用）"
                ));
                changed = true;
            }
            // 读失败/未写入：last_version 不更新（daemon 写好后新 version 再触发）。
        }

        // 3. config_epoch 变化 → 回调（与用户库注入解耦，独立热载）。
        changed |= self.on_config_epoch_consume(engine, epoch, &on_config_epoch);

        // 4. 在线翻转：离线→在线 → 重新 Register（§4.4 daemon 重启恢复）。
        let was_offline = !self.set_online(true);
        if was_offline {
            log_line("[daemon] daemon 上线翻转：重新注册工具栏实例（§4.4）");
            on_online();
        }
        changed
    }

    /// config_epoch 变化 → 回调（text_service 注入：engine.set_config + candwin.set_theme）。
    /// 返回 true = 纪元确实消费（回调已触发）。
    fn on_config_epoch_consume(
        &self,
        engine: &Arc<Engine>,
        epoch: u32,
        cb: &dyn Fn(&Arc<Engine>),
    ) -> bool {
        if epoch != *self.last_epoch.lock().unwrap_or_else(|e| e.into_inner()) {
            *self.last_epoch.lock().unwrap_or_else(|e| e.into_inner()) = epoch;
            log_line(&format!("[daemon] 配置纪元 {epoch}：触发配置热载"));
            cb(engine);
            true
        } else {
            false
        }
    }

    /// 发管道写请求：UserMutation → Request → 发送。失败重连一次。
    /// 返回 true = daemon 已接受（引擎跳过本地写盘）。
    pub fn apply_mutation(&self, m: &UserMutation) -> bool {
        let req = user_mutation_to_request(m);
        match self.send_request(&req) {
            Some(resp) => {
                if response_ok(&resp) {
                    true
                } else {
                    if let Response::Err { msg } = &resp {
                        log_line(&format!(
                            "[daemon] 写请求被守护进程拒绝（降级本地写盘）：{msg}"
                        ));
                    }
                    false
                }
            }
            None => false,
        }
    }

    /// 单笔管道请求（连接→发送→用后即弃，失败重连一次）：服务端一连接只服务一请求
    /// （accept → serve → DisconnectNamedPipe，刻意以公平服务多进程客户端），缓存复用
    /// 会把下笔请求打到已断开的句柄（实测 0x800700E9 ERROR_PIPE_BROKEN，靠重连自愈
    /// 但多一次失败往返）——故成功/失败重试后一律丢连接，下次请求新建。
    ///
    /// `silent_connect`：连接失败时**不记日志**（`ensure_daemon` 的存活探测用——
    /// daemon 未运行时"无管道"是常态，不值得刷噪声；普通写请求传 false 记录降级）。
    fn request_once(&self, req: &Request, silent_connect: bool) -> Option<Response> {
        let mut pipe = self.pipe.lock().unwrap_or_else(|e| e.into_inner());
        if pipe.is_none() {
            match PipeClient::connect() {
                Ok(c) => *pipe = Some(c),
                Err(e) => {
                    if !silent_connect {
                        log_line(&format!("[daemon] 写请求连接失败（降级本地写盘）：{e}"));
                    }
                    return None;
                }
            }
        }
        let client = pipe.as_ref().expect("已连接（上方刚置位）");
        match client.request(req) {
            Ok(resp) => {
                // 服务端已断开本连接（一请求一连接）→ 丢缓存，下次请求新建。
                *pipe = None;
                Some(resp)
            }
            Err(e) => {
                // 连接断开：清缓存 + 重连一次（daemon 可能刚重启）。
                *pipe = None;
                if !silent_connect {
                    log_line(&format!("[daemon] 写请求发送失败（重连一次）：{e}"));
                }
                match PipeClient::connect() {
                    Ok(c) => {
                        let resp = c.request(req);
                        // 无论成败：连接用后即弃，下次新建。
                        *pipe = None;
                        resp.ok()
                    }
                    Err(e2) => {
                        if !silent_connect {
                            log_line(&format!("[daemon] 重连失败（降级本地写盘）：{e2}"));
                        }
                        None
                    }
                }
            }
        }
    }

    /// 通用请求（超时/失败 → None）。发送失败 → 清缓存重连一次；用后即弃（`request_once`）。
    pub fn send_request(&self, req: &Request) -> Option<Response> {
        self.request_once(req, false)
    }

    /// 在线/离线翻转记日志（幂等）。返回**切换前**状态（调用方据此判断"上线翻转"）。
    fn set_online(&self, online: bool) -> bool {
        let mut cur = self.online.lock().unwrap_or_else(|e| e.into_inner());
        let prev = *cur;
        if *cur != online {
            *cur = online;
            if online {
                log_line("[daemon] daemon 上线：用户库走共享段引用 + 管道写");
            } else {
                log_line(&format!(
                    "[daemon] daemon 离线：写路径降级本地写盘（{}）",
                    self.user_path.display()
                ));
            }
        }
        prev
    }

    /// daemon 存活检测（管道 Ping）：连接复用；连接失败（daemon 未建管道）/Ping
    /// 失败（连接断开）→ 清缓存返回 false。**静默**（不记"写请求失败"噪声日志——
    /// 由 `request_once` 的 `silent_connect=true` 承担）。Ping 成功后丢连接（一请求一连接）。
    fn pipe_online(&self) -> bool {
        self.request_once(&Request::Ping, true).is_some()
    }

    /// 查询工具栏全局显隐偏好（语言栏右键菜单项文案用，菜单打开频度极低可承受一次
    /// 管道往返）。daemon 离线 / 旧版 daemon（未知请求 → Err 应答）→ None（调用方回退中性文案）。
    pub fn toolbar_visible(&self) -> Option<bool> {
        match self.send_request(&Request::GetToolbarVisible) {
            Some(Response::ToolbarVisible { visible }) => Some(visible),
            _ => None,
        }
    }

    // ===== 32-status-toolbar.md §4.1 TSF→daemon 上报（Register/StateSync/Active/Unregister） =====

    // ===== 工具条信号通道（40-toolbar-show-hide-governance.md 纯信号模型）=====
    //
    // 三消息走专用管道 `iuv-toolbar-signal`（与数据面物理隔离）：激活(+四态)/失焦/
    // 态变更。发送失败仅弃缓存连接（下次发送前重连）；daemon 重启后任意焦点事件
    // 自然重建一切——零恢复机制。

    /// 激活上报：实例获得焦点 + 当前四态（daemon 绑定并渲染工具栏）。
    pub fn focus_gained(&self, pid: u32, tid: u32, state: ImeState) {
        self.send_signal(&ToolbarSignal::FocusGained { pid, tid, state });
    }

    /// 失焦上报：实例失去焦点（daemon 解绑并隐藏工具栏）。
    pub fn focus_lost(&self, pid: u32, tid: u32) {
        self.send_signal(&ToolbarSignal::FocusLost { pid, tid });
    }

    /// 态变更上报：会话中途四态变化（daemon 刷新绑定实例图标）。
    pub fn state_changed(&self, pid: u32, tid: u32, state: ImeState) {
        self.send_signal(&ToolbarSignal::StateChanged { pid, tid, state });
    }

    /// 发送信号（连接缓存复用；未连/断开 → 先重连一次；仍失败 → 记日志放弃，
    /// 下次焦点事件自然补上终态）。
    fn send_signal(&self, sig: &ToolbarSignal) {
        let mut g = self.signal.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            match SignalClient::connect() {
                Ok(c) => *g = Some(c),
                Err(e) => {
                    log_line(&format!("[signal] 连接失败（daemon 不在线）：{e}"));
                    return;
                }
            }
        }
        if let Some(c) = g.as_ref() {
            if let Err(e) = c.send(sig) {
                *g = None;
                log_line(&format!("[signal] 发送失败（弃缓存，下次重连）：{e}"));
            }
        }
    }
}

impl UserRemote for DaemonClient {
    /// 引擎写路径回调：发管道写请求。false（离线/拒绝）→ 引擎降级本地写盘兜底。
    fn apply(&self, m: &UserMutation) -> bool {
        self.apply_mutation(m)
    }
}

/// UserMutation（引擎侧）→ 管道 Request（iuv-data ipc.rs 编码表）。
/// 与 UserDict 方法一一对应：Swap/Set/Remove/Block；Swap 的 a_eff/b_eff 即
/// Request::Swap 的 a_adj/b_adj（合成权重绝对值）。
pub(crate) fn user_mutation_to_request(m: &UserMutation) -> Request {
    match m {
        UserMutation::Swap {
            a_code,
            a_word,
            a_eff,
            b_code,
            b_word,
            b_eff,
        } => Request::Swap {
            a_code: a_code.clone(),
            a_word: a_word.clone(),
            a_adj: *a_eff,
            b_code: b_code.clone(),
            b_word: b_word.clone(),
            b_adj: *b_eff,
        },
        UserMutation::Set { code, word, adj } => Request::Set {
            code: code.clone(),
            word: word.clone(),
            adj: *adj,
        },
        UserMutation::Remove { code, word } => Request::Remove {
            code: code.clone(),
            word: word.clone(),
        },
        UserMutation::Block { code, word } => Request::Block {
            code: code.clone(),
            word: word.clone(),
        },
    }
}

/// Response 判定（纯函数，供测试/发送路径复用）：Ok → daemon 已接受。
pub(crate) fn response_ok(resp: &Response) -> bool {
    matches!(resp, Response::Ok { .. })
}

/// 自启冷却（进程级）：距上次尝试 ≥ `LAUNCH_COOLDOWN_SECS` 才允许再次拉起。
/// 通过则更新上次尝试时间戳。纯函数化便于单测（注入 now）。
fn launch_cooldown_ok() -> bool {
    launch_cooldown_ok_at(now_unix_secs())
}

fn launch_cooldown_ok_at(now: u64) -> bool {
    static LAST_ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let last = LAST_ATTEMPT.load(Ordering::Relaxed);
    if now.saturating_sub(last) < LAUNCH_COOLDOWN_SECS {
        return false;
    }
    // 乐观更新：并发窗口内多进程同时通过（可接受，daemon 互斥兜底）。
    let _ = LAST_ATTEMPT.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed);
    true
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// DLL 同目录 `iuv-daemon.exe` 的路径（安装目录 = %ProgramFiles%\iuv\，dev 同目录成立）。
/// 不存在 → None。
fn daemon_exe_path() -> Option<std::ffi::OsString> {
    let dll = crate::registration::dll_path();
    if dll.is_empty() {
        return None;
    }
    let exe = std::path::PathBuf::from(&dll)
        .parent()?
        .join("iuv-daemon.exe");
    if exe.is_file() {
        Some(exe.into_os_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::Config;
    use iuv_data::{Dict, UserDict};
    use iuv_win::ShmWriter;

    /// 自启冷却：60s 内第二次尝试被拒；期满放行。
    #[test]
    fn launch_cooldown_gates_attempts() {
        // 静态 LAST_ATTEMPT 可能被其他测试污染——用大时间差保证首次必然放行。
        let t0 = now_unix_secs().saturating_add(1_000_000);
        assert!(launch_cooldown_ok_at(t0), "首次尝试放行");
        assert!(!launch_cooldown_ok_at(t0 + 10), "10s 内冷却：拒绝");
        assert!(!launch_cooldown_ok_at(t0 + 59), "59s 内冷却：拒绝");
        assert!(launch_cooldown_ok_at(t0 + 61), "期满放行");
    }

    /// 请求映射纯函数：UserMutation → Request 一一对应。
    #[test]
    fn user_mutation_to_request_mapping() {
        assert_eq!(
            user_mutation_to_request(&UserMutation::Swap {
                a_code: "haoshi".into(),
                a_word: "好使".into(),
                a_eff: 5800,
                b_code: "haoshi".into(),
                b_word: "耗时".into(),
                b_eff: 3800,
            }),
            Request::Swap {
                a_code: "haoshi".into(),
                a_word: "好使".into(),
                a_adj: 5800,
                b_code: "haoshi".into(),
                b_word: "耗时".into(),
                b_adj: 3800,
            }
        );
        assert_eq!(
            user_mutation_to_request(&UserMutation::Set {
                code: "zhang'wei'wei".into(),
                word: "张葳葳".into(),
                adj: 8000,
            }),
            Request::Set {
                code: "zhang'wei'wei".into(),
                word: "张葳葳".into(),
                adj: 8000,
            }
        );
        assert_eq!(
            user_mutation_to_request(&UserMutation::Remove {
                code: "de".into(),
                word: "的".into(),
            }),
            Request::Remove {
                code: "de".into(),
                word: "的".into(),
            }
        );
        assert_eq!(
            user_mutation_to_request(&UserMutation::Block {
                code: "shou'xuan".into(),
                word: "手癣".into(),
            }),
            Request::Block {
                code: "shou'xuan".into(),
                word: "手癣".into(),
            }
        );
    }

    /// Response 判定：Ok → true（daemon 接受）；Err → false（降级）。
    #[test]
    fn response_judgement() {
        assert!(response_ok(&Response::Ok { version: 42 }));
        assert!(!response_ok(&Response::Err {
            msg: "写盘失败".into()
        }));
    }

    /// 共享段是会话级命名对象：daemon_client 的段相关测试与 shm.rs 测试共享同一段，
    /// 串行执行 + 容忍段上既有状态（版本/纪元可能已被其他测试/进程写过）。
    static SEG_LOCK: Mutex<()> = Mutex::new(());

    /// poll：版本变化注入用户库 + version 未变不重复注入。
    #[cfg(windows)]
    #[test]
    fn poll_injects_user_dict_on_version_change() {
        let _g = SEG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut w = ShmWriter::create_or_open().unwrap();
        w.write(&UserDict::empty().set_entry("da", "龘", 7))
            .unwrap();
        let engine = Arc::new(Engine::new(
            Dict::from_entries(vec![("da".into(), "龘".into(), 100000)]),
            Config::default(),
        ));
        let client = DaemonClient::new(std::env::temp_dir().join("poll-inject.imedic"));
        // 首 poll：注入共享段用户库（覆盖 base 权重 100000 → 7）
        assert!(client.poll(&engine, |_| {}, || {}), "首次应有变化");
        let user = engine.user_dict().expect("poll 注入后用户库应装配");
        assert_eq!(user.adjusted("da"), vec![("龘".to_string(), 7)]);
        // version 未变：不再注入/不再有变化
        assert!(!client.poll(&engine, |_| {}, || {}), "同 version 无变化");
        // 再写新库（version+1）→ 重新注入
        w.write(&UserDict::empty().set_entry("da", "龘", 8))
            .unwrap();
        assert!(client.poll(&engine, |_| {}, || {}), "新版本应再注入");
        let user = engine.user_dict().expect("再次注入后用户库");
        assert_eq!(user.adjusted("da"), vec![("龘".to_string(), 8)]);
    }

    /// poll：config_epoch 变化触发回调一次（同纪元不再触发）。
    #[cfg(windows)]
    #[test]
    fn poll_fires_config_reload_on_epoch_change() {
        let _g = SEG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut w = ShmWriter::create_or_open().unwrap();
        let engine = Arc::new(Engine::new(Dict::default(), Config::default()));
        let client = DaemonClient::new(std::env::temp_dir().join("poll-epoch.imedic"));
        // 首 poll：容忍段上既有纪元（可能 >0），记录已触发次数基线
        let fired = std::sync::atomic::AtomicUsize::new(0);
        client.poll(
            &engine,
            |_| {
                fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            || {},
        );
        let base = fired.load(std::sync::atomic::Ordering::SeqCst);
        // bump 一次 → 触发一次
        w.bump_config_epoch();
        client.poll(
            &engine,
            |_| {
                fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            || {},
        );
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            base + 1,
            "纪元变化触发一次"
        );
        // 同纪元再 poll → 不触发
        client.poll(
            &engine,
            |_| {
                fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            || {},
        );
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            base + 1,
            "同纪元不再触发"
        );
    }

    /// 离线路径不 panic（假设 daemon 未运行/管道名固定不可假名——仅断言分支可达）。
    #[cfg(windows)]
    #[test]
    fn offline_degrade_paths() {
        let client = DaemonClient::new(std::env::temp_dir().join("offline.imedic"));
        let engine = Arc::new(Engine::new(Dict::default(), Config::default()));
        let _ = client.poll(&engine, |_| {}, || {});
    }
}
