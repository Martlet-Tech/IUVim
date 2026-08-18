//! 引擎：候选生成。契约 01-contract.md §4 engine.rs / §4.2 算法。

use crate::{
    rerank::RerankCtx, schema::Quanpin, session::Session, store::NullStore, Config, InputSchema,
    LmProvider, RerankStage, UnigramLm, UserDataStore,
};
use iuv_data::{Dict, Entry, UserDict};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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

/// 引擎：进程级单例，跨线程共享。
pub struct Engine {
    pub(crate) dict: Dict,
    /// 配置（Mutex：M6 设置页热载 engine.set_config 需要 &self 内部可变）。
    pub(crate) config: Mutex<Config>,
    pub(crate) schema: Box<dyn InputSchema>,
    pub(crate) lm: Box<dyn LmProvider>,
    pub(crate) stages: Vec<Box<dyn RerankStage>>,
    pub(crate) store: Mutex<Box<dyn UserDataStore>>,
    /// 用户权重覆盖表状态（M2 主动调权，18-m2-user-dict.md）：路径 + 上次加载 mtime
    /// （会话创建时检测跨进程写入的延迟生效；M6 daemon 模式关闭，见 reload_user_dict）。
    user_state: Mutex<UserState>,
    /// 用户库远端写后端（M6 daemon 客户端）。None = 本地写盘（现状/降级）。
    /// apply 返回 false（daemon 离线/拒绝）→ 写路径自动降级本地，绝不挂键。
    user_remote: Mutex<Option<Arc<dyn UserRemote>>>,
}

/// 用户库装配状态（不可变路径 + 可变 mtime 基线）。
#[derive(Default)]
struct UserState {
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
}

/// 候选生成档位（契约 §4.2 路由表的一等概念）：输入经 `Engine::classify` 归入
/// 微软实测的 5 档 + 空档，`generate_candidates` 按档位分派，后续加档（M3 模糊音
/// 等）= 加一个 Route 臂，不再往分派函数里叠 if。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// 严格前缀（`c`/`sh`/`zho`）→ 纯单字（单字桶）
    PrefixChars,
    /// 完整单音节无歧义（`shi`/`de`/`ba`）→ 纯单字（exact 全量）
    CompleteChars,
    /// 完整单音节有歧义（`xian`→[xi,an]）→ 全拼整句/词条两通道（替代切分词混排）
    AmbiguousSyllable,
    /// 多段全完整（`nihao`/`xi'an`）**或末音节可补全**（`shigechengy`→`y` 补 `yu/yi/…`）
    /// → 全拼两通道：整句通道唯一一次 Viterbi（2a 直接 / 2b 补全） + 词条通道砍尾巴 exact
    FullPinyin,
    /// 多段全不完整（`nh`/`nhm`）→ 简拼键逐级砍尾巴
    Abbrev,
    /// 多段混合且**中段不完整**（`nhao`）→ 简拼段展开配对（无整句通道）
    Mixed,
    /// 单段非前缀（`i`/`u`/`v`）或空输入 → 无候选（生成末尾有原文兜底，见 generate_candidates）
    Empty,
}

impl Engine {
    /// 默认装配：Quanpin + UnigramLm + [StaticOrder] + NullStore。
    pub fn new(dict: Dict, config: Config) -> Arc<Engine> {
        let syllables = dict.syllables().clone();
        let lm = UnigramLm::new(dict.total_weight(), dict.entry_count());
        Self::with_parts(
            dict,
            config,
            Box::new(Quanpin::new(syllables)),
            Box::new(lm),
            vec![Box::new(crate::rerank::StaticOrder)],
            Box::new(NullStore),
        )
    }

    /// 全注入构造器（测试与后续里程碑用）。
    pub fn with_parts(
        dict: Dict,
        config: Config,
        schema: Box<dyn InputSchema>,
        lm: Box<dyn LmProvider>,
        stages: Vec<Box<dyn RerankStage>>,
        store: Box<dyn UserDataStore>,
    ) -> Arc<Engine> {
        Arc::new(Engine {
            dict,
            config: Mutex::new(config),
            schema,
            lm,
            stages,
            store: Mutex::new(store),
            user_state: Mutex::new(UserState::default()),
            user_remote: Mutex::new(None),
        })
    }

    pub fn start_session(self: &Arc<Self>) -> Session {
        self.reload_user_dict();
        Session::new(self.clone())
    }

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
    fn reload_user_dict(&self) {
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

    /// M6 配置热载（config_epoch 变化 → 设置页保存后触发）：全量替换引擎配置。
    /// 读取点（page_size/max_candidates/candidate_prefix/candidate_orientation/
    /// passthrough_apps/theme）随 `config()` 新值即时生效；TSF 侧键位 keymap 映射
    /// 装配不在此热切（M7 键位热载范畴，见 22-m6-daemon.md；调用方自行记日志）。
    pub fn set_config(&self, config: Config) {
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
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
        let ps = self
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .page_size
            .max(1);
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

    /// 当前配置快照（克隆：M6 热载后新值立即可见；读侧不用锁穿透引用）。
    pub fn config(&self) -> Config {
        self.config.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 调试/REPL 用精确查询。
    pub fn lookup(&self, squashed_code: &str) -> Vec<Entry> {
        self.dict.exact(squashed_code)
    }

    /// 档位判定（唯一判定点）：`plain` 为去 `'` 的整串，`plans` 为全部切分方案
    /// （消费端遍历所有方案，2026-08-14：多段判定不再只看贪心方案[0]）。
    /// 与契约 §4.2 路由表逐行对应：
    /// - 整串是音节真前缀 → 单字档（切分器可能把它切成多段，但微软实测只出单字）
    /// - 单段完整音节：无替代切分 → 单字档；有替代切分（xian）→ 全拼（混排西安）
    /// - 单段非前缀（i/u/v）→ 空
    /// - 多段：**存在任一全完整方案 → 全拼**（dier 贪心 [die,r] 是 Mixed，但 [di,er]
    ///   全完整应走全拼枚举——否则「第二」不可达，实测 2026-08-14）；**末音节可补全
    ///   （除末段外全完整 + 末段为音节前缀）→ 也归全拼 2b**（shigechengy → 补 y）；
    ///   否则按段完整性分派（全不完整 → 简拼；混合 → 混拼，仅中段不完整如 nhao）
    fn classify(&self, plain: &str, seg: &[String], plans: &[Vec<String>]) -> Route {
        if !plain.is_empty() && self.is_syllable_prefix(plain) && !self.is_syllable(plain) {
            return Route::PrefixChars;
        }
        if seg.len() == 1 {
            if self.is_syllable(&seg[0]) {
                return if plans.len() > 1 {
                    Route::AmbiguousSyllable
                } else {
                    Route::CompleteChars
                };
            }
            return Route::Empty;
        }
        let any_full = plans
            .iter()
            .any(|p| p.iter().all(|s| !s.is_empty() && self.is_syllable(s)));
        // 末音节可补全（2026-08-18）：除末段外全为完整音节 + 末段为音节前缀
        // （`shigechengy` → 末段 `y`，可补 yu/yi/yang/…）→ 归入全拼档走 2b 整句通道，
        // 词条通道按输入砍（砍 `y` 而非补 `yu`），与 2a 路径砍完第一刀后前缀对齐。
        // 否则 `shigechengy` 会被误判 Mixed（扩展笛卡尔）——2b 场景丢失。
        let tail_completable = seg.len() >= 2
            && seg[..seg.len() - 1]
                .iter()
                .all(|s| !s.is_empty() && self.is_syllable(s))
            && {
                let last = seg.last().unwrap();
                !last.is_empty() && !self.is_syllable(last) && self.is_syllable_prefix(last)
            };
        if any_full || tail_completable {
            Route::FullPinyin
        } else if seg.iter().all(|s| !s.is_empty() && !self.is_syllable(s)) {
            Route::Abbrev
        } else {
            Route::Mixed
        }
    }

    /// 消费端方案重排（2026-08-14）：方案[0] = 词频最优而非贪心——分节显示与主路径
    /// 跟随用户最可能打的词（keneng → ke'neng 可能；dier → di'er 第二）。
    /// 排序键 = 方案 join 键 exact 词条最大权重（词条优先；无词条 = 0），
    /// 稳定排序保贪心原序。切分函数零改动（消费端遍历所有方案）。
    pub(crate) fn rank_plans(&self, plans: Vec<Vec<String>>) -> Vec<Vec<String>> {
        if plans.len() <= 1 {
            return plans;
        }
        let mut scored: Vec<(u32, usize)> = plans
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let key = p.join("'");
                let w = self.dict.exact(&key).first().map(|e| e.weight).unwrap_or(0);
                (w, i)
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| plans[i].clone()).collect()
    }

    /// 按契约 §4.2 生成候选。
    ///
    /// 路由（M1.5，微软实测对齐，见 docs/research/msime-probe-checklist.txt）：
    /// - 整串为音节前缀（`c`/`sh`/`zho`）→ 纯单字（单字桶，词频序）
    /// - 完整单音节：无歧义（`shi`）→ 纯单字（exact 全量）；歧义（`xian`→[xi,an]）→ 全拼两通道
    /// - 单段非前缀（`i`/`u`/`v`）→ 无候选（末尾兜底：原文候选，见 generate_candidates）
    /// - 多段全完整（`nihao`/`xi'an`）**或末音节可补全**（`shigechengy`）→ 全拼两通道
    ///   （整句通道唯一一次 Viterbi 2a/2b + 词条通道砍尾巴 exact）
    /// - 多段全不完整（`nh`/`nhm`/`nhmsx`）→ 简拼键逐级砍尾巴（构建期键，O(1) exact）
    /// - 多段混合（`nhao`，中段不完整）→ 不完整段展开音节配对查询（上限内，超限降级）
    ///
    /// 部分消费：候选 seg_len=k，选中间级词经 session 悬空续接把尾巴重建为组合。
    pub(crate) fn generate_candidates(
        &self,
        raw: &str,
        seg: &[String],
        plans: &[Vec<String>],
    ) -> Vec<crate::Candidate> {
        let plain: String = raw.chars().filter(|c| *c != '\'').collect();
        let mut cands = match self.classify(&plain, seg, plans) {
            Route::PrefixChars => self.single_segment_candidates(&plain),
            Route::CompleteChars => self.single_segment_candidates(&seg[0]),
            Route::AmbiguousSyllable | Route::FullPinyin => {
                self.full_pinyin_candidates(&raw, &plain, seg)
            }
            Route::Abbrev => self.abbrev_candidates(seg),
            Route::Mixed => self.mixed_candidates(seg),
            Route::Empty => Vec::new(),
        };

        // 前缀补全（联想）：默认关闭（微软化，候选仅 exact）；config 开启时追加。
        //    用方案[0] 的 `'` 键做前缀匹配（词库键已分隔化）。
        //    联想词消费全部当前段（seg_len = n），选中即整词上屏。
        // 配置快照：单点锁取克隆（RerankCtx 需 &Config；热载 set_config 与读取并发安全）。
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if cfg.candidate_prefix {
            let n = seg.len();
            let squashed = seg.join("'");
            for e in &self.dict.prefix(&squashed, 20) {
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    n,
                ));
            }
        }

        // 按 text 去重（保序，先见先留）
        let mut seen = std::collections::HashSet::new();
        cands.retain(|c| seen.insert(c.text.clone()));

        // 截断到 max_candidates
        cands.truncate(cfg.max_candidates);

        // 依次过 stages 管线
        let now = SystemTime::now();
        let store = self.store.lock().expect("store lock poisoned");
        let ctx = RerankCtx {
            raw,
            seg,
            store: store.as_ref(),
            config: &cfg,
            now,
        };
        for stage in &self.stages {
            stage.rerank(&ctx, &mut cands);
        }

        // 兜底：所有路由均无候选且输入非空 → 原文候选（"不认识"语义：`input`/`window`/`i`
        // 等无法命中任何词库的输入，窗口内容不空、可 1/Space 直接上屏原文）。
        // 复用现有 Word/Char 惯例（多字符 Word、单字符 Char），不新增候选类型；
        // 无编号呈现由 UI 按 text == 预编辑原文 判定。seg_len = 段数 → 全消费、会话结束。
        if cands.is_empty() && !plain.is_empty() {
            let kind = if plain.chars().count() >= 2 {
                crate::CandidateKind::Word
            } else {
                crate::CandidateKind::Char
            };
            cands.push(crate::Candidate::new(
                plain.clone(),
                kind,
                plain.clone(),
                0,
                seg.len(),
            ));
        }
        cands
    }

    pub(crate) fn is_syllable(&self, s: &str) -> bool {
        self.dict.syllables().contains(s)
    }

    fn is_syllable_prefix(&self, s: &str) -> bool {
        self.dict.syllables().iter().any(|syl| syl.starts_with(s))
    }

    /// 单段档：完整音节或音节前缀 → 纯单字（词频序）。空档（i/u/v）由 classify 拦截。
    /// 微软实测：单段输入无论完整与否只出单字（shi→是时十使，无"时间/时候"）。
    /// 数据源分两路（M1.5 修正）：完整音节 → `dict.exact_single` 全部同音字（首字母桶
    /// 混收多字词会把同音字挤出 top-N，如 shi 只剩 5 字）；严格前缀 → 单字桶
    /// （桶只收单字，前缀无法 exact）。
    /// 候选**全量返回不截断**（微软对齐：sh 候选 600+ 全给、翻页可达，见
    /// docs/research/msime-probe-checklist.txt G3），由全局 max_candidates 兜底。
    fn single_segment_candidates(&self, s: &str) -> Vec<crate::Candidate> {
        if s.is_empty() {
            return Vec::new();
        }
        let entries: Vec<iuv_data::Entry> = if self.is_syllable(s) {
            self.dict.exact_single(s)
        } else {
            // 严格前缀：单字桶（桶只收单字，过滤 starts_with）
            let first = s.chars().next().unwrap();
            self.dict
                .initial_top(first, iuv_data::INITIAL_BUCKET_SIZE)
                .into_iter()
                .filter(|e| e.code.starts_with(s))
                .collect()
        };
        entries
            .into_iter()
            .map(|e| crate::Candidate::new(e.word, crate::CandidateKind::Char, e.code, e.weight, 1))
            .collect()
    }

    /// 简拼键档：多段全不完整（`nh`/`nhm`/`nhmsx`）→ 构建期简拼键逐级砍尾巴。
    /// 每级 k：exact(前 k 段首字母串)；尾巴段由 session 悬空续接重建为组合。
    /// 微软实测：简拼只出词（纯 exact 匹配，无单字、无更长词前缀）。
    fn abbrev_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
        const PER_LEVEL_EXACT: usize = 20;
        let n = seg.len();
        let mut cands = Vec::new();
        for k in (1..=n).rev() {
            let key: String = seg[..k]
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| s.as_str())
                .collect();
            if key.is_empty() {
                continue;
            }
            let mut pushed = 0usize;
            for e in &self.dict.exact(&key) {
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    k,
                ));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }
        }
        cands
    }

    /// 混拼档：多段混合（`nhao` → n 简拼 + hao 完整）→ 不完整段展开为音节列表，
    /// 逐级笛卡尔积 exact 查询（词频合并）；组合数超限该级降级为空。
    fn mixed_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
        const PER_LEVEL_EXACT: usize = 20;
        const MAX_EXPAND_QUERIES: usize = 2000;
        let n = seg.len();
        let mut cands = Vec::new();
        for k in (1..=n).rev() {
            // 展开前 k 段：完整段→自身；不完整段→音节前缀列表
            let mut lists: Vec<Vec<&str>> = Vec::new();
            let mut product: usize = 1;
            let mut ok = true;
            for s in &seg[..k] {
                if s.is_empty() {
                    ok = false;
                    break;
                }
                if self.is_syllable(s) {
                    lists.push(vec![s.as_str()]);
                } else {
                    let l: Vec<&str> = self
                        .dict
                        .syllables()
                        .iter()
                        .filter(|syl| syl.starts_with(s))
                        .map(|x| x.as_str())
                        .collect();
                    if l.is_empty() {
                        ok = false;
                        break;
                    }
                    product *= l.len();
                    if product > MAX_EXPAND_QUERIES {
                        ok = false;
                        break;
                    }
                    lists.push(l);
                }
            }
            if !ok {
                continue; // 该级降级为空
            }
            // 笛卡尔积 → exact 查询 → 词频合并
            let mut combos: Vec<Vec<&str>> = vec![Vec::new()];
            for l in &lists {
                let mut next = Vec::with_capacity(combos.len() * l.len());
                for c in &combos {
                    for syl in l {
                        let mut cc = c.clone();
                        cc.push(syl);
                        next.push(cc);
                    }
                }
                combos = next;
            }
            let mut entries: Vec<iuv_data::Entry> = Vec::new();
            for combo in &combos {
                entries.extend(self.dict.exact(&combo.join("'")));
            }
            entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
            let mut seen = std::collections::HashSet::new();
            let mut pushed = 0usize;
            for e in entries {
                if !seen.insert(e.word.clone()) {
                    continue;
                }
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    k,
                ));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }
        }
        cands
    }

    /// 整句通道（2026-08-18，词库负责"词"、Viterbi 只负责"唯一最佳句子"）。
    /// 对未选中部分至多产出**一条** Sentence：
    /// - **2a**：末段为完整音节（`…chengyu`）→ 整串跑一次 Viterbi；
    /// - **2b**：末段为音节前缀（`…chengy` 的 `y`）→ 把末段补全为所有合法音节
    ///   （`y` → yu/yi/yang/ying/…），**每个补齐方案各跑一次** Viterbi；
    /// - 全部句子按 viterbi 路径分取最高一条（M2 屏蔽组合拦截）。
    /// 前置守卫：除末段外其余段必须全为完整音节（Mixed 中段不完整如 `nhao` 不组句）。
    /// 2b 句子文本可能超出已敲字母（`shigechengy` → 「是一个成语」），预编辑显示仍按
    /// 输入切分不扩展（session 预览规则，见 session.rs candidate_preview 第 3 条）。
    fn sentence_candidates(&self, vseg: &[String], cfg: &Config) -> Option<crate::Candidate> {
        if vseg.len() < 2 {
            return None;
        }
        let (last, rest) = vseg.split_last()?;
        if !rest.iter().all(|s| self.is_syllable(s)) {
            return None;
        }
        let sentences: Vec<(crate::Candidate, f64)> = if self.is_syllable(last) {
            // 2a：结尾完整音节 → 唯一一次 Viterbi
            crate::viterbi::best_sentence_scored(&self.dict, vseg, &*self.lm, cfg)
                .into_iter()
                .collect()
        } else {
            // 2b：末段补全为所有合法音节，逐个补齐跑 Viterbi
            self.dict
                .syllables()
                .iter()
                .filter(|s| s.starts_with(last.as_str()))
                .filter_map(|comp| {
                    let mut s = rest.to_vec();
                    s.push(comp.clone());
                    crate::viterbi::best_sentence_scored(&self.dict, &s, &*self.lm, cfg)
                })
                .collect()
        };
        let best = sentences
            .into_iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;
        let blocked = self
            .dict
            .user()
            .map(|u| u.is_blocked(&best.0.code, &best.0.text))
            .unwrap_or(false);
        if blocked {
            None
        } else {
            Some(best.0)
        }
    }

    /// 现有全拼路径（含歧义单音节与末音节可补全 2b 场景）：两通道。
    /// **整句通道**（唯一）：`sentence_candidates` 一次 Viterbi（2a）/逐补齐一次（2b），
    /// 至多一条 Sentence，排最前。
    /// **词条通道**：k=n..1 砍末音节——合法砍完整音节（`…chengyu` 砍 `yu`）、不合法砍
    /// 不完整段（`…chengy` 砍 `y`，两路砍完第一刀后前缀对齐）——对剩余前缀枚举切分查
    /// exact（词/单字），命中才录；k=1 追加单字全量。排序 = 整句 → exact 匹配长度从长到短
    /// → 同 k 按权重降序。viterbi.rs 算法零改动。
    fn full_pinyin_candidates(
        &self,
        raw: &str,
        plain: &str,
        seg: &[String],
    ) -> Vec<crate::Candidate> {
        // 每级词候选上限（"2/3 字词时几个/十几个候选词"的规模；全局另有 max_candidates 截断）。
        const PER_LEVEL_EXACT: usize = 20;

        // 配置快照（viterbi 需 &Config；热载 set_config 与读取并发安全）。
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let mut cands: Vec<crate::Candidate> = Vec::new();
        let n = seg.len();

        // 1. 整句通道：唯一一次 Viterbi（至多一条 Sentence），置候选最前。
        //    2a 结尾合法 → 整串一次；2b 结尾不合法 → 末段逐补齐一次，取分最高。
        //    不因切分方案、不因 k 消费长度产生多条整句（2026-08-18 重写）。
        let vseg: Vec<String> = seg.iter().filter(|s| !s.is_empty()).cloned().collect();
        if let Some(sentence) = self.sentence_candidates(&vseg, &cfg) {
            cands.push(sentence);
        }

        // 2. 词条通道：砍尾巴逐级 exact（k=n..1，命中才录）。raw 含撇号（强制输入
        //    xi'an/fen'ge）保持 join 键不枚举，强制语义不破。
        for k in (1..=n).rev() {
            let prefix = &seg[..k];

            // 前缀枚举切分 → exact 词/单字（join 键）。枚举源必须是无撇号的 plain 前缀：
            //    多段方案 join(') 后段内枚举会被 `'` 强制切分扼杀（fenge 只出 feng'e、
            //    keneng 只出 ken'eng，fen'ge/ke'neng 不可及）；raw 含撇号（强制输入如
            //    xi'an/fen'ge）保持 join 键不枚举，强制语义不破。
            let consumed_chars: usize = prefix.iter().map(|s| s.len()).sum();
            let mut keys = vec![prefix.join("'")];
            if !raw.contains('\'') {
                for plan in self
                    .schema
                    .segment(&plain[..consumed_chars.min(plain.len())])
                {
                    keys.push(plan.join("'"));
                }
            }
            let mut entries: Vec<iuv_data::Entry> = Vec::new();
            for key in keys {
                entries.extend(self.dict.exact(&key));
            }
            entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
            let mut seen = std::collections::HashSet::new();
            let mut pushed = 0usize;
            for e in entries {
                if !seen.insert(e.word.clone()) {
                    continue;
                }
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(e.word, kind, e.code, e.weight, k));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }

            // 3. 最后一级（k=1，第一段单字）**追加单字全量**（微软对齐：多段输入翻页
            //    可达低频同音字，如 zhangweiwei→选张→weiwei 续接翻页取「葳」；原
            //    PER_LEVEL_EXACT=20 把低频字卡在边界，实测 2026-08-14）。追加而非
            //    替换：歧义单音节（xian→[xi,an]）的枚举替代切分词（西安）必须保留，
            //    重复单字由 generate_candidates 末尾全局 text 去重兜底（保序先见先留）。
            //    单段档语义：完整音节 → exact_single 全量；严格前缀 → 首字母桶。
            if k == 1 {
                cands.extend(self.single_segment_candidates(&seg[0]));
            }
        }

        cands
    }

    /// 用户选择记录（commit 时调用）。
    pub(crate) fn record_selection(&self, code: &str, text: &str) {
        let mut store = self.store.lock().expect("store lock poisoned");
        store.record_selection(code, text, SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Key;
    use iuv_data::Dict;

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

    /// set_config：配置热载替换引擎配置（M6 config_epoch 触发路径）。
    #[test]
    fn set_config_updates_engine_config() {
        let e = Engine::new(swap_dict(), Config::default());
        assert_eq!(e.config().page_size, 5);
        let cfg = Config {
            page_size: 7,
            passthrough_apps: vec!["dota2.exe".into()],
            ..Config::default()
        };
        e.set_config(cfg.clone());
        assert_eq!(e.config().page_size, 7);
        assert_eq!(e.config().passthrough_apps, vec!["dota2.exe".to_owned()]);
        assert_eq!(e.config().keymap, cfg.keymap, "未改字段保持新值");
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
