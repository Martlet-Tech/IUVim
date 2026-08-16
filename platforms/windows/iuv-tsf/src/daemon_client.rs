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
use std::sync::{Arc, Mutex};

use iuv_core::{Engine, UserMutation, UserRemote};
use iuv_data::{PipeClient, Request, Response, ShmReader};

use crate::log::log_line;

/// M6 daemon 客户端。单实例经 `Arc` 与引擎 `user_remote` 共享（text_service 持一份、
/// 引擎持一份），全部方法 `&self`（内部 Mutex）。
pub struct DaemonClient {
    /// 命名管道连接缓存（惰性：首次 `daemon_online`/`send_request` 时 connect；
    /// 发送失败/断开 → 清缓存，下次重连）。
    pipe: Mutex<Option<PipeClient>>,
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
            shm: Mutex::new(None),
            last_version: Mutex::new(0),
            last_epoch: Mutex::new(0),
            user_path,
            online: Mutex::new(false),
        }
    }

    /// 检测 daemon 在线：管道 connect + Ping。成功缓存句柄；连接失败 / Ping 超时 → false。
    /// daemon 崩溃后连接断开 → 清缓存（下次重连），返回 false。
    pub fn daemon_online(&self) -> bool {
        let mut pipe = self.pipe.lock().unwrap_or_else(|e| e.into_inner());
        if pipe.is_none() {
            match PipeClient::connect() {
                Ok(c) => *pipe = Some(c),
                Err(e) => {
                    log_line(&format!("[daemon] 管道连接失败（daemon 不在线）：{e}"));
                    return false;
                }
            }
        }
        let client = pipe.as_ref().expect("已连接（上方刚置位）");
        match client.request(&Request::Ping) {
            Ok(Response::Ok { .. }) => true,
            Ok(Response::Err { msg }) => {
                // 守护进程在线但异常：仍视为在线（写请求会再次判定，Err → 降级）。
                log_line(&format!("[daemon] Ping 响应 Err：{msg}"));
                true
            }
            Err(e) => {
                log_line(&format!("[daemon] Ping 失败（连接断开，清缓存）：{e}"));
                *pipe = None;
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
    /// 4. 在线状态翻转记日志；**不**把引擎 user_remote 置 None（apply 返回 false 即降级，
    ///    天然兜底）。
    ///
    /// 返回 true = 用户库或配置有变化（调用方无需特殊处理，仅日志/断言用）。
    pub fn poll(&self, engine: &Arc<Engine>, on_config_epoch: impl Fn(&Arc<Engine>)) -> bool {
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
                log_line(&format!("[daemon] 用户库版本 {version}：注入引擎（共享段只读引用）"));
                changed = true;
            }
            // 读失败/未写入：last_version 不更新（daemon 写好后新 version 再触发）。
        }

        // 3. config_epoch 变化 → 回调（与用户库注入解耦，独立热载）。
        changed |= self.on_config_epoch_consume(engine, epoch, &on_config_epoch);

        self.set_online(true);
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
                        log_line(&format!("[daemon] 写请求被守护进程拒绝（降级本地写盘）：{msg}"));
                    }
                    false
                }
            }
            None => false,
        }
    }

    /// 通用请求（超时/失败 → None）。连接缺失时先尝试连接；发送失败 → 清缓存重连一次。
    pub fn send_request(&self, req: &Request) -> Option<Response> {
        let mut pipe = self.pipe.lock().unwrap_or_else(|e| e.into_inner());
        if pipe.is_none() {
            match PipeClient::connect() {
                Ok(c) => *pipe = Some(c),
                Err(e) => {
                    log_line(&format!("[daemon] 写请求连接失败（降级本地写盘）：{e}"));
                    return None;
                }
            }
        }
        let client = pipe.as_ref().expect("已连接（上方刚置位）");
        match client.request(req) {
            Ok(resp) => Some(resp),
            Err(e) => {
                // 连接断开：清缓存 + 重连一次（daemon 可能刚重启）。
                *pipe = None;
                log_line(&format!("[daemon] 写请求发送失败（重连一次）：{e}"));
                match PipeClient::connect() {
                    Ok(c) => {
                        let resp = c.request(req);
                        *pipe = Some(c);
                        resp.ok()
                    }
                    Err(e2) => {
                        log_line(&format!("[daemon] 重连失败（降级本地写盘）：{e2}"));
                        None
                    }
                }
            }
        }
    }

    /// 在线/离线翻转记日志（幂等）。
    fn set_online(&self, online: bool) {
        let mut cur = self.online.lock().unwrap_or_else(|e| e.into_inner());
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

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::Config;
    use iuv_data::{Dict, ShmWriter, UserDict};

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
        w.write(&UserDict::empty().set_entry("de", "的", 7))
            .unwrap();
        let engine = Arc::new(Engine::new(
            Dict::from_entries(vec![("de".into(), "的".into(), 100000)]),
            Config::default(),
        ));
        let client = DaemonClient::new(std::env::temp_dir().join("poll-inject.imedic"));
        // 首 poll：注入共享段用户库（覆盖 base 权重 100000 → 7）
        assert!(client.poll(&engine, |_| {}), "首次应有变化");
        assert_eq!(engine.lookup("de")[0].weight, 7, "共享段用户库注入引擎");
        // version 未变：不再注入/不再有变化
        assert!(!client.poll(&engine, |_| {}), "同 version 无变化");
        // 再写新库（version+1）→ 重新注入
        w.write(&UserDict::empty().set_entry("de", "的", 8))
            .unwrap();
        assert!(client.poll(&engine, |_| {}), "新版本应再注入");
        assert_eq!(engine.lookup("de")[0].weight, 8);
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
        client.poll(&engine, |_| {
            fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let base = fired.load(std::sync::atomic::Ordering::SeqCst);
        // bump 一次 → 触发一次
        w.bump_config_epoch();
        client.poll(&engine, |_| {
            fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            base + 1,
            "纪元变化触发一次"
        );
        // 同纪元再 poll → 不触发
        client.poll(&engine, |_| {
            fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            base + 1,
            "同纪元不再触发"
        );
    }

    /// 编译期接线：DaemonClient 可作 UserRemote（&self apply 经内部 Mutex 转发）。
    /// 返回值环境相关（daemon 运行中 → true；未运行 → false），只断言不 panic + 类型接线。
    #[test]
    fn daemon_client_is_user_remote() {
        let client = DaemonClient::new(std::env::temp_dir().join("remote-trait.imedic"));
        let _ = UserRemote::apply(
            &client,
            &UserMutation::Set {
                code: "de".into(),
                word: "的".into(),
                adj: 5,
            },
        );
    }

    /// 离线路径不 panic（假设 daemon 未运行/管道名固定不可假名——仅断言分支可达）。
    #[cfg(windows)]
    #[test]
    fn offline_degrade_paths() {
        let client = DaemonClient::new(std::env::temp_dir().join("offline.imedic"));
        let engine = Arc::new(Engine::new(Dict::default(), Config::default()));
        let _ = client.poll(&engine, |_| {});
    }
}