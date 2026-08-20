//! 用户库写路径（M2 主动调权/自造词/隐藏 + M6 daemon 远端后端），见
//! 18-m2-user-dict.md / 22-m6-daemon.md。与 iuv-data::UserDict（纯数据层）分工：
//! 本模块 = 引擎侧装配/持久化/远端分派（Engine 的 userdict 相关 impl 块）。

use crate::engine::Engine;
use iuv_data::UserDict;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// 用户库写操作（M6 daemon 管道请求的引擎侧视图，与 UserDict 方法一一对应，
/// 见 18-m2-user-dict.md §Swap/Set/Remove/Block）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserMutation {
    /// Shift+←/→ 主动调权：a/b 两词**互写对方合成权重**（绝对值覆盖，双 code 签名，
    /// 对应 UserDict::apply_swap）。
    Swap {
        a_code: String,
        a_word: String,
        a_eff: u32,
        b_code: String,
        b_word: String,
        b_eff: u32,
    },
    /// 自造词/覆盖写入（upsert，对应 UserDict::set_entry）。
    Set { code: String, word: String, adj: u32 },
    /// 移除用户库条目（隐藏自造词/覆盖 = 撤销自造，对应 UserDict::remove_entry）。
    Remove { code: String, word: String },
    /// 屏蔽基础库词条（Shift+Delete 隐藏，对应 UserDict::block）。
    Block { code: String, word: String },
}

/// 用户库远端写后端（M6 daemon 模式，见 22-m6-daemon.md §3）。
/// 返回 `true` = 远端已接受（本进程无需写盘）；`false` = 未接受（降级本地写盘兜底）。
pub trait UserRemote: Send + Sync {
    fn apply(&self, m: &UserMutation) -> bool;
}

/// 用户库装配状态（不可变路径 + 可变 mtime 基线）。
#[derive(Default)]
pub struct UserState {
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
}

impl Engine {
    /// 装配用户权重覆盖表（M2 主动调权）。
    /// **任何失败都降级为空库继续**（用户库不允许阻断输入法）：缺失/损坏 → 空
    /// UserDict + 路径照常记录（后续 swap 写盘时创建/重建文件）。返回 `Err` 仅
    /// 供调用方记日志，**不代表未装配**。
    pub fn attach_user_dict(&self, path: PathBuf) -> std::io::Result<()> {
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        let (user, err) = match UserDict::load(&path) {
            Ok(u) => (u, None),
            Err(e) => (UserDict::empty(), Some(e)),
        };
        self.dict.set_user(Arc::new(user));
        *self.user_state.lock().unwrap_or_else(|e| e.into_inner()) = UserState {
            path: Some(path),
            mtime,
        };
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// 用户库 mtime 检测重载（跨进程延迟生效：其他进程写盘后，本进程新会话拿到新库）。
    /// 文件变化 → 重载成功替换；失败（删除窗口/损坏）→ 保持旧库。
    /// **M6 daemon 模式整体关闭**：共享段轮询（DaemonClient::poll）已接管读路径（版本检测
    /// 注入），mtime 重载与之双写冲突；daemon 离线时用户库唯一写者（守护进程）不在线，
    /// 内存态经 install_user 保持自洽，关闭无害。
    pub(crate) fn reload_user_dict(&self) {
        if self
            .user_remote
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return;
        }
        let state = self.user_state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(path) = state.path.clone() else {
            return;
        };
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime == state.mtime {
            return;
        }
        drop(state);
        if let Ok(user) = UserDict::load(&path) {
            self.dict.set_user(Arc::new(user));
        }
        *self.user_state.lock().unwrap_or_else(|e| e.into_inner()) = UserState {
            path: Some(path),
            mtime,
        };
    }

    /// M2 主动调权：a/b 两词**交换有效权重**（绝对值覆盖，互写对方合成权重）。
    /// 双 code：候选页内相邻词可能跨 code（单段档桶候选 sha/shi…同属 `sh`）。
    /// 任一不在基本库（如整句候选）→ 忽略（防御即可）。
    /// 写盘失败不阻断：内存态已生效（本次会话立即重排），持久化下次调整时重试。
    pub(crate) fn swap_weights(&self, a_code: &str, a_word: &str, b_code: &str, b_word: &str) {
        let Some(a_base) = self
            .dict
            .exact(a_code)
            .into_iter()
            .find(|e| e.word == a_word)
            .map(|e| e.weight)
        else {
            return;
        };
        let Some(b_base) = self
            .dict
            .exact(b_code)
            .into_iter()
            .find(|e| e.word == b_word)
            .map(|e| e.weight)
        else {
            return;
        };
        let user = self.dict.user();
        let eff = |code: &str, word: &str, base: u32| -> u32 {
            user.as_ref()
                .and_then(|u| {
                    u.adjusted(code)
                        .iter()
                        .find(|(w, _)| w == word)
                        .map(|(_, a)| *a)
                })
                .unwrap_or(base)
        };
        let a_eff = eff(a_code, a_word, a_base);
        let b_eff = eff(b_code, b_word, b_base);
        let next = if let Some(u) = user.as_deref() {
            u.apply_swap(a_code, a_word, b_eff, b_code, b_word, a_eff)
        } else {
            UserDict::empty().apply_swap(a_code, a_word, b_eff, b_code, b_word, a_eff)
        };
        let mutation = UserMutation::Swap {
            a_code: a_code.to_string(),
            a_word: a_word.to_string(),
            a_eff,
            b_code: b_code.to_string(),
            b_word: b_word.to_string(),
            b_eff,
        };
        self.install_user(next, Some(&mutation));
    }

    /// 替换用户库内存态并持久化（写盘失败不阻断：内存态已生效，下次调整重试）。
    /// mtime 基线同步刷新（防止本进程下次会话对自写文件无谓重载）。
    ///
    /// M6 remote 分支：携带 `mutation` 时先问远端（daemon 客户端）；远端 `apply` 返回
    /// `true` → 跳过本地写盘（内存态照常替换——本地即时生效，共享段周期重读会覆盖为
    /// 一致态）；远端失败/无 remote → 现状本地写盘兜底（降级路径必须保留）。
    fn install_user(&self, next: UserDict, mutation: Option<&UserMutation>) {
        if let Some(m) = mutation {
            if let Some(remote) = self
                .user_remote
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                if remote.apply(m) {
                    self.dict.set_user(Arc::new(next));
                    return;
                }
            }
        }
        let mut state = self.user_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = &state.path {
            if next.save(path).is_ok() {
                state.mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
            }
        }
        drop(state);
        self.dict.set_user(Arc::new(next));
    }

    /// 装配/更换用户库远端写后端（M6 daemon 客户端；`None` 拆除 = 回归本地写盘）。
    /// 重复调用幂等（只替换 Arc 引用）；daemon 掉线后 apply 返回 false 即自动降级本地。
    pub fn set_user_remote(&self, remote: Option<Arc<dyn UserRemote>>) {
        *self.user_remote.lock().unwrap_or_else(|e| e.into_inner()) = remote;
    }

    /// 注入用户库内存态（M6：会话进程从共享段重读后调用）。
    /// 与 attach_user_dict 语义区分：**只 set_user**，不动 mtime 基线、不写盘、
    /// 不触碰本地文件——共享段是唯一读源。
    pub fn set_user_dict(&self, user: Arc<UserDict>) {
        self.dict.set_user(user);
    }

    /// 当前用户库（M2/M6 只读视图；None = 未装配）。
    pub fn user_dict(&self) -> Option<Arc<UserDict>> {
        self.dict.user()
    }

    /// M2 自造词记录（逐字选择 commit 时调用，18-m2-user-dict.md）：场景 0/a/b 权重判定。
    /// - 0：词库（含用户库）已有整词 → 跳过（幂等：重复自造被拦截，权重不漂移）
    /// - a：无命中 → 常量 `PHRASE_DEFAULT_WEIGHT`
    /// - b：n 条命中 → 目标位 = 首页最后一位：n ≥ page_size → avg(cand[ps-2], cand[ps-1])
    ///   （u64 计算防溢出）；n < page_size → cand[n-1] − 1（saturating 防 0 下溢）
    pub(crate) fn record_phrase(&self, code: &str, text: &str) {
        const PHRASE_DEFAULT_WEIGHT: u32 = 8000;
        let entries = self.dict.exact(code); // 叠加视图（含用户库独有条目）
        if entries.iter().any(|e| e.word == text) {
            return; // 场景 0
        }
        let ps = self.page_size() as usize;
        let n = entries.len();
        let w = if n == 0 {
            PHRASE_DEFAULT_WEIGHT
        } else if n < ps {
            entries[n - 1].weight.saturating_sub(1)
        } else {
            ((entries[ps - 2].weight as u64 + entries[ps - 1].weight as u64) / 2) as u32
        };
        let next = match self.dict.user() {
            Some(u) => u.set_entry(code, text, w),
            None => UserDict::empty().set_entry(code, text, w),
        };
        let mutation = UserMutation::Set {
            code: code.to_string(),
            word: text.to_string(),
            adj: w,
        };
        self.install_user(next, Some(&mutation));
    }

    /// M2 隐藏候选（Shift+Delete）：先删用户库条目（自造词/覆盖），
    /// 否则屏蔽基础库词条。写盘失败不阻断（内存态已生效）。
    pub(crate) fn hide_entry(&self, code: &str, text: &str) {
        let (next, mutation) = match self.dict.user() {
            Some(u) if u.adjusted(code).iter().any(|(w, _)| w == text) => (
                u.remove_entry(code, text),
                UserMutation::Remove {
                    code: code.to_string(),
                    word: text.to_string(),
                },
            ),
            Some(u) => (
                u.block(code, text),
                UserMutation::Block {
                    code: code.to_string(),
                    word: text.to_string(),
                },
            ),
            None => (
                UserDict::empty().block(code, text),
                UserMutation::Block {
                    code: code.to_string(),
                    word: text.to_string(),
                },
            ),
        };
        self.install_user(next, Some(&mutation));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Key};
    use iuv_data::Dict;
    use std::sync::Mutex;

    fn dict_of(items: Vec<(&str, &str, u32)>) -> Dict {
        Dict::from_entries(
            items
                .into_iter()
                .map(|(c, w, wt)| (c.into(), w.into(), wt))
                .collect(),
        )
    }

    fn user_weight(e: &Engine, code: &str, word: &str) -> Option<u32> {
        e.dict.user().and_then(|u| {
            u.adjusted(code)
                .iter()
                .find(|(w, _)| w == word)
                .map(|(_, a)| *a)
        })
    }

    #[test]
    fn record_phrase_weight_scenarios() {
        // a：无命中 → 8000
        let e = Engine::new(
            dict_of(vec![
                ("zhang".into(), "张", 90000),
                ("wei".into(), "威", 1000),
                ("wei".into(), "葳", 50),
            ]),
            Config::default(),
        );
        e.record_phrase("zhang'wei'wei", "张葳葳");
        assert_eq!(
            user_weight(&e, "zhang'wei'wei", "张葳葳"),
            Some(8000),
            "场景 a 常量权重"
        );

        // b1：n=2 < page_size=5 → 手癣(300)−1 = 299
        let e = Engine::new(
            dict_of(vec![
                ("shou'xuan".into(), "首选", 8000),
                ("shou'xuan".into(), "手癣", 300),
            ]),
            Config::default(),
        );
        e.record_phrase("shou'xuan", "手选");
        assert_eq!(
            user_weight(&e, "shou'xuan", "手选"),
            Some(299),
            "b1：n−1 位减一"
        );

        // b2：n=6 >= page_size=5 → avg(中芯3000, 众心1000) = 2000
        let e = Engine::new(
            dict_of(vec![
                ("zhong'xin".into(), "中心", 9000),
                ("zhong'xin".into(), "衷心", 7000),
                ("zhong'xin".into(), "钟鑫", 5000),
                ("zhong'xin".into(), "中芯", 3000),
                ("zhong'xin".into(), "众心", 1000),
                ("zhong'xin".into(), "忠信", 800),
            ]),
            Config::default(),
        );
        e.record_phrase("zhong'xin", "中信");
        assert_eq!(
            user_weight(&e, "zhong'xin", "中信"),
            Some(2000),
            "b2：avg(第4, 第5位)"
        );

        // 场景 0：词库已有 → 跳过（不记录）
        let e = Engine::new(
            dict_of(vec![("zhang'wei'wei".into(), "张威威", 6000)]),
            Config::default(),
        );
        e.record_phrase("zhang'wei'wei", "张威威");
        assert!(
            user_weight(&e, "zhang'wei'wei", "张威威").is_none(),
            "场景 0 不记录"
        );

        // 幂等：重复自造（用户库已有）→ 场景 0 拦截，权重不漂移
        e.record_phrase("zhang'wei'wei", "张威威");
        assert!(user_weight(&e, "zhang'wei'wei", "张威威").is_none());
        let e2 = Engine::new(
            dict_of(vec![
                ("shou'xuan".into(), "首选", 8000),
                ("shou'xuan".into(), "手癣", 300),
            ]),
            Config::default(),
        );
        e2.record_phrase("shou'xuan", "手选");
        e2.record_phrase("shou'xuan", "手选"); // 第二次：exact 含用户库手选 → 跳过
        assert_eq!(
            user_weight(&e2, "shou'xuan", "手选"),
            Some(299),
            "重复自造权重不漂移"
        );
    }

    #[test]
    fn hide_entry_removes_override_then_blocks() {
        // 用户库有条目（自造词）→ 隐藏 = 删除条目
        let e = Engine::new(
            dict_of(vec![
                ("shou'xuan".into(), "首选", 8000),
                ("shou'xuan".into(), "手癣", 300),
            ]),
            Config::default(),
        );
        e.record_phrase("shou'xuan", "手选");
        assert!(user_weight(&e, "shou'xuan", "手选").is_some());
        e.hide_entry("shou'xuan", "手选");
        assert!(
            user_weight(&e, "shou'xuan", "手选").is_none(),
            "隐藏自造词 = 删除条目"
        );
        assert!(
            !e.dict.user().unwrap().is_blocked("shou'xuan", "手选"),
            "删除分支不写屏蔽"
        );
        // 无用户库条目（基础库词）→ 屏蔽
        e.hide_entry("shou'xuan", "手癣");
        assert!(
            e.dict.user().unwrap().is_blocked("shou'xuan", "手癣"),
            "基础库词 → 屏蔽"
        );
        // 屏蔽词条 + 整句拦截：exact 与 viterbi 都不再出现（集成测试已验证候选层）
        let hits = e.dict.exact("shou'xuan");
        assert!(!hits.iter().any(|x| x.word == "手癣"));
    }

    // ===== M6 远端写后端（UserRemote）：daemon 模式写路径 =====

    /// 测试远端：记录收到的 mutation，按 `accepted` 返回成功/失败。
    struct FakeRemote {
        accepted: bool,
        calls: Mutex<Vec<UserMutation>>,
    }

    impl UserRemote for FakeRemote {
        fn apply(&self, m: &UserMutation) -> bool {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(m.clone());
            self.accepted
        }
    }

    fn swap_dict() -> Dict {
        dict_of(vec![("de".into(), "的", 100000), ("de".into(), "得", 300)])
    }

    /// 远端接受 → 跳过本地写盘（内存态照常替换 + mutation 构造正确）。
    #[test]
    fn set_user_remote_skips_file_write_when_accepted() {
        let path = std::env::temp_dir().join(format!("iuv-remote-ok-{}.imedic", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let e = Engine::new(swap_dict(), Config::default());
        let _ = e.attach_user_dict(path.clone()); // 路径已记录（首次无文件：空库）
        let remote = Arc::new(FakeRemote {
            accepted: true,
            calls: Mutex::new(Vec::new()),
        });
        e.set_user_remote(Some(remote.clone()));
        e.swap_weights("de", "的", "de", "得");
        // 内存态立即生效（本地即时；共享段周期重读覆盖为一致态）
        assert!(user_weight(&e, "de", "得").is_some(), "内存态应更新");
        assert_eq!(
            user_weight(&e, "de", "的"),
            Some(300),
            "互写对方合成权重"
        );
        // 远端接受 → 本地不写盘
        assert!(!path.exists(), "远端接受后不应写盘，实际文件存在");
        // mutation 构造正确（Swap 双 code + 合成权重）
        let calls = remote.calls.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            UserMutation::Swap {
                a_code: "de".into(),
                a_word: "的".into(),
                a_eff: 100000,
                b_code: "de".into(),
                b_word: "得".into(),
                b_eff: 300,
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 远端拒绝（daemon 离线/报错）→ 降级本地写盘兜底（现状 install_user 语义保留）。
    #[test]
    fn set_user_remote_rejected_falls_back_to_local_write() {
        let path = std::env::temp_dir().join(format!("iuv-remote-no-{}.imedic", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let e = Engine::new(swap_dict(), Config::default());
        let _ = e.attach_user_dict(path.clone());
        let remote = Arc::new(FakeRemote {
            accepted: false,
            calls: Mutex::new(Vec::new()),
        });
        e.set_user_remote(Some(remote));
        e.swap_weights("de", "的", "de", "得");
        assert!(path.exists(), "远端拒绝 → 本地写盘兜底，实际无文件");
        let loaded = iuv_data::UserDict::load(&path).unwrap();
        assert!(
            loaded.adjusted("de").iter().any(|(w, a)| w == "得" && *a == 100000),
            "本地写盘内容应为交换后的合成权重"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 远端接受 → record_phrase/hide_entry 同样跳过本地写盘（Set/Remove/Block 构造正确）。
    #[test]
    fn remote_mode_record_and_hide_skip_write() {
        let path = std::env::temp_dir().join(format!("iuv-remote-ops-{}.imedic", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let e = Engine::new(
            dict_of(vec![("shou'xuan".into(), "首选", 8000)]),
            Config::default(),
        );
        let _ = e.attach_user_dict(path.clone());
        let remote = Arc::new(FakeRemote {
            accepted: true,
            calls: Mutex::new(Vec::new()),
        });
        e.set_user_remote(Some(remote.clone()));
        // 自造词 → Set
        e.record_phrase("shou'xuan", "手选");
        // 隐藏自造词（用户库有条目）→ Remove
        e.hide_entry("shou'xuan", "手选");
        // 隐藏基础库词 → Block
        e.hide_entry("shou'xuan", "首选");
        assert!(!path.exists(), "远端接受全程不写盘");
        let calls = remote.calls.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[0],
            UserMutation::Set {
                code: "shou'xuan".into(),
                word: "手选".into(),
                adj: 7999,
            }
        );
        assert_eq!(
            calls[1],
            UserMutation::Remove {
                code: "shou'xuan".into(),
                word: "手选".into(),
            }
        );
        assert_eq!(
            calls[2],
            UserMutation::Block {
                code: "shou'xuan".into(),
                word: "首选".into(),
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    /// set_user_dict：只注入内存态，不写盘不动 mtime 基线（共享段重读路径）。
    #[test]
    fn set_user_dict_injects_without_write() {
        let path = std::env::temp_dir().join(format!("iuv-inject-{}.imedic", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let e = Engine::new(swap_dict(), Config::default());
        let _ = e.attach_user_dict(path.clone());
        let user = iuv_data::UserDict::empty().set_entry("de", "的", 5);
        e.set_user_dict(Arc::new(user));
        assert_eq!(user_weight(&e, "de", "的"), Some(5), "内存态注入生效");
        assert!(!path.exists(), "set_user_dict 不写盘");
        let _ = std::fs::remove_file(&path);
    }

    /// mtime 重载在远端模式整体关闭（poll 接管读路径；避免与共享段双写冲突）。
    #[test]
    fn reload_user_dict_disabled_in_remote_mode() {
        let path = std::env::temp_dir().join(format!("iuv-reload-{}.imedic", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let e = Engine::new(swap_dict(), Config::default());
        let _ = e.attach_user_dict(path.clone());
        // 远端模式装配
        e.set_user_remote(Some(Arc::new(FakeRemote {
            accepted: true,
            calls: Mutex::new(Vec::new()),
        })));
        // 外部进程改写文件（本地路径的 mtime 变化源）
        let ext = iuv_data::UserDict::empty().apply_swap("de", "的", 100, "de", "地", 999999);
        ext.save(&path).unwrap();
        // 新会话触发 reload_user_dict：远端模式应跳过 → 不重载外部内容
        let mut s = e.start_session();
        for c in "de".chars() {
            s.on_key(Key::Char(c));
        }
        assert!(
            !e.dict.exact("de").iter().any(|x| x.word == "地"),
            "远端模式 mtime 重载应关闭（共享段轮询接管）"
        );
        let _ = std::fs::remove_file(&path);
    }
}