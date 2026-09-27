# 49 · 架构重构：薄客户端 + 引擎服务端（M10 立项）

> 状态：**协议定稿**（§6 各项已拍板，P0 实测后开工）。
> 一句话：把「每个应用进程一份引擎」改成「全系统一个引擎服务进程 + 薄 TSF 客户端」，
> 重划模块分层、清算历史包袱。


## 1. 目标 / 非目标

1. **输入流畅**——每键处理预算内、无逐键抖动；
2. **候选合理**——候选质量不回退（rime 管线是刚收敛的资产，**算法内核原样保留**，只换宿主）；
3. **响应迅速**——宿主进程首键无可感延迟；TSF DLL 轻到可忽略。

非目标：不换候选算法、不做 M3 功能（整句增强/模糊音/emoji/学习候选，P2 后基于新架构做）、
本期不实现 mac/linux 前端（只验证分层预留）、不动 IMEDIC02/IUVUSR02 数据格式。

## 2. 目标架构

```
┌──────────── 宿主应用进程（每应用一份，极薄）────────────┐
│ iuv-tsf (cdylib)                                       │
│   COM/TSF 管线 · composition 桥 · ui_element 数据桥     │
│   langbar 中英按钮 · proto 客户端（管道传输）            │
│   无词库 · 无字体 · 无引擎 · 不读盘                      │
└───────────────┬────────────────────────────────────────┘
                │ 命名管道 iuv.service.v1（每 TSF 实例一条连接）
                │ 上行：按键/焦点/光标矩形  下行：动作+UI状态推送
┌───────────────┴────────────────────────────────────────┐
│ iuv-server.exe（全系统一份，登录自启 + 按需拉起）          │
│   会话管理器（每客户端实例一个会话）· 引擎(rime) · 词库    │
│   用户库（进程内直写，唯一真相源）· 配置热载               │
│   候选窗/菜单渲染（tiny-skia+cosmic-text，字体只此一份）   │
│   工具栏 · egui 设置页                                    │
└─────────────────────────────────────────────────────────┘
```

## 3. 模块分层（三个维度）

三个正交维度：**平台维度**（纯 Rust / 平台壳）、**逻辑维度**（数据 / 算法 / 编排 / 展现）、
**进程维度**（server / client / 工具）。每个 crate 只占一格，依赖单向向下。

### 3.1 新 workspace 布局

```
crates/（平台无关，纯 Rust）
  iuv-proto     IPC 协议：帧格式 + 消息类型 + serde 编解码 —— 线上契约唯一权威
  iuv-dict      数据层：IMEDIC02 格式 + Dict mmap 查询 + UserDict + OpenCC + dictc（现 iuv-data 收敛）
  iuv-engine    算法内核：rime 管线 + Quanpin + UnigramLm + 标点/全角表 + Engine 参数子集
                —— 无 IO、无线程、无时钟依赖，repl 可直驱
  iuv-service   应用编排：会话管理器（重写）、Config 全量 + 文件 IO、用户库事务、资源装配、paths
  iuv-ui        展现层：tiny-skia + cosmic-text + Theme（仅 server 与工具进程链接）
  iuv-pet       宠物模型/物理/皮肤（自 iuv-core 出走；iuv-ui 与 server 消费）
  iuv-repl      调试 CLI：直驱 engine + --connect 连 server（协议一致性手测）
platforms/windows/
  iuv-tsf       薄客户端 cdylib：COM/TSF + composition + ui_element 桥 + proto client
                依赖收窄为：iuv-proto + iuv-win + windows
  iuv-server    服务进程 exe：service + engine + dict + ui + egui 设置页 + 工具栏（daemon 演进）
  iuv-win       Win32 原语：ULW + popup + 管道传输（iuv-proto 的 Windows transport）
```

依赖白名单（新增需批准，沿用现行约定）：
iuv-proto 仅 serde；iuv-dict 仅 serde；iuv-engine 仅 iuv-dict + iuv-proto（类型）；
iuv-service 仅 iuv-proto/dict/engine + serde_json；iuv-ui 仅 tiny-skia/cosmic-text + iuv-proto（Snapshot 消费）；
iuv-tsf 仅 iuv-proto + iuv-win + windows 系。**线上类型（Key/Effect/PageInfo/四态）唯一定义在
iuv-proto，engine 与 client 共同消费——Effect 契约原样升级为线上协议。**

---

## 4. IPC 协议（`iuv-proto`）

### 4.0 前提（对旧否决的清算）

- **推翻** 31/22 号「每键走 IPC 不可接受」：50-200µs 是估值非实测（`perf.rs` 埋点晚于该文 10 天）；
  且当年反对的是「每次转换走 IPC」（每键多次往返），新架构整引擎上移、**每键 1 次往返**（§4.5.1）；
  现路径「一请求一连接」是最差形态而非下限（46 号冻结曾到 1.83s）。
- **保留** 两条：「daemon 死 = 打字停」→ §4.5.4 必答；「忙时即弃共用管道会丢消息」（40 号）
  → 本次重写的直接动因，由 §4.6 latest-wins 从根上消除。
- 热路径 P50/P99 实测为 P0 前置，**没有数据不开工**（§5）。

### 4.1 拓扑

- 每个 TSF 实例 ↔ `iuv-server.exe` 保持**一条长连接**（命名管道 `iuv.service.v1`），
  async `select!` 单连接多路复用。不搞一请求一连接、不搞 thread-per-connection。
- 一条连接三个平面，同一套帧，只差语义与优先级：

| 平面 | 发起 | 形态 | 内容 | 替代的旧机制 |
|---|---|---|---|---|
| ① 热路径 | 客户端 | REQ/RESP，同步，有截止时间 | 按键 → 结果 | — |
| ② 控制面 | 服务端 | REQ/RESP，异步 | `Ctl`（工具栏点击） | ctl 反向通道 |
| ③ 状态面 | 服务端 | PUSH，latest-wins | 四态/配置/用户库版本 | toolbar signal + SHM 轮询 |

- 用户库写管道亦迁入（`UserMutation`）；SHM 数据面保留（§4.9）。现存 4 套 IPC 收敛完毕。

### 4.2 帧格式

```
偏移  长度  字段          说明
0     4     payload_len   u32 LE，不含 8B 头
4     1     kind          0=客户端REQ 1=服务端REQ(Ctl/Ping) 2=客户端RESP 3=服务端RESP 4=PUSH
                          （双向 REQ/RESP 分开编号，帧自描述，不依赖连接方向即可解码）
5     1     flags         bit0=URGENT（热路径帧优先出队），其余保留
6     2     stream_id     u16 LE：客户端用偶数、服务端用奇数（防双向 REQ 撞号）；
                          单调递增，回绕时跳过在途号
8     N     payload       serde 编解码（§4.3）
```

- 显式分帧，不依赖 `PIPE_TYPE_MESSAGE`：传输可替换（mac/linux 换 UDS/mach port）。
- 单帧 ≤ 64 KiB（沿用 `PIPE_FRAME_MAX`），超限 = 协议级错误，**绝不静默截断**。
- URGENT 只管排队优先级；大帧传输挡路由 §4.7 发送纪律解决，不改线格式。

### 4.3 消息（方向即类型，穷尽 match）

```rust
// —— C2S ——
#[non_exhaustive]
pub enum C2S {
    Hello { proto_min: u16, proto_max: u16, auth: Auth,
            resume: Option<ResumeToken>, client: ClientInfo }, // 首帧必为它
    Key { key: Key, mods: Mods, token: KeyToken, full: bool }, // 引擎唯一入口；full=基线失效，要求全量应答
    EndSession { end: SessionEnd },
    CaretMoved { rect: CaretRect, dpi: u16 },                  // 客户端节流后发，定位候选窗
    FocusChanged { focused: bool },
    SetMaintenance { on: bool },
    UserMutation(UserMutation),
    Ping { nonce: u32 },
    Pong { nonce: u32 },
}

// —— S2C ——
#[non_exhaustive]
pub enum S2C {
    HelloAck { proto: u16, caps: Caps, server_build: BuildId },
    KeyResult(KeyVerdict),            // Consumed(KeyOutcome) | Busy
    Ok,
    Err(ProtoError),
    Ctl { cmd: CtlCmd },              // 服务端发起，需应答
    Ping { nonce: u32 },
    Pong { nonce: u32 },
}

// —— PUSH（服务端 → 客户端，latest-wins）——
#[non_exhaustive]
pub enum Push {
    SessionAttached { token: ResumeToken }, // 会话建立时下发，重连重绑用（§4.4）
    ImeState(ImeState),
    FocusBound { focused: bool },
    ConfigChanged { epoch: u32, client_view: ClientConfig },
    UserDictChanged { version: u32 },       // 只推版本号，数据走 SHM（§4.9）
    UiElement(Effect),                      // 仅 CAP_UIELEMENT
    TypingActivity { active: bool },
    Shutdown { grace_ms: u32 },             // 唯一不可合并、必达
}

pub enum KeyVerdict {
    Consumed(KeyOutcome),
    Busy,   // 引擎过载立即回、未推进状态；客户端当场放行本键，不触发重同步
}

/// Option = 与上帧相同；会话首次应答必须全量（composition 恒 Some）。
pub struct KeyOutcome {
    pub eaten: bool,                        // OnTestKeyDown 的应答
    pub composition: Option<String>,
    pub commit: Option<String>,             // Some → 上屏并结束会话
    pub candidates: Option<Vec<Candidate>>, // 以下三项仅 CAP_UIELEMENT
    pub page: Option<PageInfo>,
    pub selected: Option<u32>,
}

pub struct KeyToken { pub seq: u32, pub phase: KeyPhase } // phase: Test/Down/Up
```

- `Candidate` 上线剔除 `score: f64`（仅诊断用）。
- 线上类型（Key/Effect/PageInfo/四态）唯一定义在 `iuv-proto`。

### 4.4 握手 / 认证 / 重绑

`Hello{proto_min, proto_max, auth, resume?} → HelloAck{proto = max(交集), caps, build}`。

- 版本无交集 → `VersionMismatch` 明确报错；**任何握手失败，客户端降级为完全透明**，绝不卡宿主。
- 认证：管道 DACL 限本登录会话 + `auth` 共享密钥（安装时生成、ACL 保护；密钥文件放
  用户配置目录、仅当前用户可读、不轮换——已拍板）。防跨账户/低权限
  冒充；同用户恶意进程超出威胁模型。
- 重绑：服务端建会话时 `Push::SessionAttached{token}` 下发令牌；断线重连 `Hello.resume`
  回绑旧会话并重放 composition（§4.5.4 方案 C 的前提）；`EndSession` 后令牌作废。
- caps 位图（`CAP_UIELEMENT` / `CAP_PET`…）：未协商的 PUSH 一律不发。

### 4.5 热路径契约 ★

**① 处理点唯一化（每键 1 次往返，继承 weasel `_fTestKeyDownPending`）**
- `OnTestKeyDown` 真正处理并缓存裁定（客户端单槽缓存：物理键 + 修饰符）；
- `OnKeyDown` 命中缓存 → 零 IPC 复用裁定；未命中（部分应用只发 OnKeyDown）→
  `Key{phase:Down}` 现场处理。
- 不去重 = 每键两次往返 + 引擎状态推进两次（现状 `key_routing.rs:123/145`）。

**② 预算与超时**
- 数字（P50/P99、心跳间隔、大帧阈值、单键硬预算）由 P0 实测定档，此处只定结构。
  **首批实测（2026-09-27，开发机，echo 64B×500，`cargo test -p iuv-win --test rtt_bench -- --nocapture`）**：
  per-request-connect（现路径形态）P50=17µs / P99≈2.0ms / mean=284µs，尾部全在 accept 间隙与建连；
  persistent（M10 目标形态）**P50=8µs / P99=13µs / max=17µs**；单次 connect=1.5ms。
  结论：长连接把 P99 从毫秒级压到 13µs（150×），传输往返不是新架构瓶颈，与 §4.5.3 瘦身判断一致。
  **真机（打字机）数据采集后回填定档**（§5 P0 出口）。
- 超时 → **放行按键 + 会话标记 degraded + 记日志**；禁止无上限重试
  （weasel `while(WaitNamedPipe)` = 服务端卡死则宿主卡死）。
- **基线失效规则**：`composition: None` 是增量语义，依赖双方基线一致。会话首次应答必须全量；
  客户端发生 Deadline/传输错误后，后续 `Key` 带 `full=true`，服务端强制全量应答；
  `Busy` 不触发（服务端未推进状态）。

**③ 载荷瘦身**
- `Effect` 全量上线太肥（`all_candidates` 长句可达数百项 = 每键几 KB）。服务端自渲染候选窗
  （§2），热路径只回 `KeyOutcome`（composition/commit），候选仅 `CAP_UIELEMENT` 走
  `Push::UiElement`。每键从几 KB 降到几十字节。

**④ 服务端失效语义（已拍板：C + A）**
- A 透明降级 / B 客户端兜底引擎（**否决**：违背「客户端无引擎」，开倒车）/
  C 断连 → 拉起 server → `ResumeToken` 重绑重放。**定案 C + A**（重生失败窗口内退化为 A）。
- 断开检测：管道断开事件为主，客户端 `Ping` 兜底半开连接。

### 4.6 推送与心跳

- 状态量（ImeState/FocusBound/ConfigChanged/UserDictChanged/UiElement/TypingActivity）
  **latest-wins**：同键合并只保最新，队列满丢旧值，**最新值必达**。`Shutdown` 唯一例外必达。
- 双向 `Ping`/`Pong`；服务端连续 N 次无 Pong → 判死 → 关连接回收会话（长连接防泄漏）。

### 4.7 安全与性能纪律（不改线格式）

- 连接数上限 + 每连接在途请求上限，超限拒绝并记日志（服务端是打字单点，防拖死）。
- **磁盘 IO（fsync/配置热载/日志）不进 async 事件循环**，一律后台线程——
  否则一次 fsync = 全系统按键抖动。
- 单键处理硬预算，超预算立即回 `Busy` 快速放行，不等客户端超时。
- 有在途热路径请求时**不启动 >8KiB 大帧写入**（URGENT 不解决传输层 HOL）。

### 4.8 错误模型

```rust
pub enum ProtoError {
    VersionMismatch { client: u16, server: u16 },
    Unauthenticated,
    FrameTooLarge { len: u32, max: u32 },
    Malformed { offset: u32, reason: &'static str }, // u32 定宽，不用 usize
    UnknownTag { kind: u8, tag: u16 },
    Deadline { op: &'static str, elapsed_ms: u32 },
    ServerGone,
}
```

纪律：解码错误 = 拒整帧 + 上报，不猜不截断（沿用 `codec.rs finish()` 拒残留的严格性）。

### 4.9 不走协议的数据

| 数据 | 机制 | 理由 |
|---|---|---|
| 用户库全量 | SHM 保留，协议只推 `UserDictChanged{version}` | 批量数据走管道是根本性错误；Release/Acquire 写序是既有资产 |
| 词库 / OpenCC 表 | 进程本地加载 | 零 IPC |
| 候选窗渲染 | 服务端本地自渲染 | 字体只此一份 |
| 大块文本 / 日志 | 不传 | 各自进程文件 |

### 4.10 与小狼毫对照（速查）

- **借鉴**：命令集全覆盖（另补 Maintenance）、Test 结果缓存、有界帧、同步 RPC 语义（加截止时间）。
- **改造**：thread-per-connection → async 单连接多路复用；`RECT` 位压缩 → 直接传结构体；
  超限截断 → 报错。
- **拒绝**：`while(WaitNamedPipe)` 无上限重试、无版本号、错误与成功返回值不可区分。
- **补齐**：版本协商、认证、心跳/会话 GC、`Busy` 快速失败。
- **反向保留**：iuv 的 SHM 是活的且有写序保证，不学 weasel 废弃 SHM；
  一实例一连接（不用 weasel 单管道 `session_id` 复用）。
- **codec**：全量 serde derive（拍板：不为「零依赖」的绝对化约束牺牲维护性）；
  二进制 format 选型 P1 定。帧头/分帧仍手写；拒残留/截断纪律由帧校验 + 测试保证（§4.8）。

---

## 5. 分期落地（建议）

| 期 | 内容 | 出口条件 |
|---|---|---|
| **P0 前置** | 热路径往返**实测**（`iuv-win::ipc::rtt` 基准 + tsf `ipc_rtt` 埋点，已落地）+ §4.5.2 数字定档 | 开发机首批数据已有；**真机 P50/P99 采集回填后开工 P2** |
| P1 | `iuv-proto` 独立成 crate：帧格式 + 三枚举 + serde codec + 往返/拒绝测试 | 与传输无关，可先落地 |
| P2 | transport（`iuv-win::transport`，std 线程 + overlapped）+ 握手/版本协商/认证（§4.4）——**已落地**（2026-09-27，7 项集成测试） | 双侧互连成功、版本不匹配有明确报错、未认证连接被拒 ✅ |
| P3 | 热路径打通（§4.5.1 去重 + §4.5.3 瘦身 + 截止时间 + 超时后全量重同步）。**P3a 已落地**（2026-09-27）：`iuv-server.exe`（引擎托管 + KeyOutcome 瘦身 + 增量基线 + full 重同步 + UiElement 过渡推送）+ 6 项无头测试；**P3b 已落地**（2026-09-27）：TSF 改薄客户端——`com/remote_host.rs`（进程级 RemoteHandle：
Test/KeyDown 单槽去重、20ms 截止、Deadline→degraded→full 重同步、断线透明降级 A、
四态同步、客户端配置副本），config `use_engine_server` 开关 A/B 切换（默认 false=现状）。
路由判定（keymap/passthrough/全角/标点）**留客户端**；待真机打字回归 | 能打字（真机），往返数 = 1/键；服务端侧契约已全测 |
| P4 | 四套旧 IPC 收敛至协议；`daemon_poll_tick` 从按键路径**删除**（改 PUSH） | 按键路径零轮询 |
| P5 | §4.5.4 失效语义落地（方案 C+A，依赖 §4.4 `ResumeToken` 重绑） | 杀服务端进程仍能优雅降级/重生 |

**P4 值得单独强调**：现状「所有自愈路径都汇聚在按键驱动的 poll，**不打字不恢复**」
（`docs/status.md:126`）是**已记录在案的缺陷**。改为 PUSH 后，该缺陷**自然消失**——
这本身就是「一条连接三平面」相对「4 套机制」的收益证明。

## 6. 拍板记录（2026-09-27）

1. **失效语义：C + A**（正常路径快速重生，重生失败窗口内透明降级；B 兜底引擎否决）。
2. **超时行为：放行按键**（不消费）；全量重同步规则随之生效（§4.5.2）。
3. **Effect 瘦身承认**（热路径不回传全量候选，服务端自渲染）；`page`/`selected`
   仅 `CAP_UIELEMENT`，暂留观察，普通客户端确认无消费方后可再砍。
4. **密钥分发**：token 文件放用户配置目录、仅当前用户可读、不轮换。
5. **codec：全量 serde**，放弃「iuv-proto 零依赖」的绝对化约束（依赖白名单已改为仅 serde）。
