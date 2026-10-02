# 50 · 全仓库品质检查（M10 后首次，新特性前置）

> 状态：**结案（2026-10-02）**——全部批次修复完成并真机回归通过：
> §1 高危 H1-H5；§2.1 transport 五项；§2.2 四项快赢 + toolbar 同步 dispatch；
> §2.3 Activate 回滚/deactivate 清会话/reviving 闸挂号/重试差别化；
> §3 死代码清扫 D1-D3（`71c2098`，净 -1100 行）；§3.2 文档对账（00-overview/
> tsf-interaction/README/AGENTS/Cargo.toml/.gitignore）。末批台账见 status.md 末条。
> 有意不修的欠账：langbar/wnd_proc panic guard、
> 部分代码内注释漂移（文中已标注）。
> 全量回归：fmt 干净、clippy 0 警告、非 shm 测试全绿（shm 三测试需管理员终端）；
> 真机：两轮优雅停机全部客户端 ~1.5s 自动恢复，零 panic/零未果。
> 方式：cargo fmt/clippy/test 全仓自动化 + 6 路模块代理逐文件通读（core/data、server、
> tsf、win+proto、ui+repl、横切脚本/文档）+ 高危项人工逐行核实。
> 范围：main @ e9f2dc1，全仓库 ~31k 行 Rust（8 crate）+ scripts + docs，
> 不以 6b349172c 以来的 diff 为限（架构已大改，按现状审）。
> 结论：整体工程素养高（注释密度、真机教训留痕、回归钉测试、锁中毒统一恢复），
> 但 **M10 迁移收尾不彻底**——死代码、双真相源、文档漂移成体系出现；
> 另核实 5 个高危 bug。新特性开工前建议按 §5 顺序清账。

## 1. 高危（5 个，全部人工逐行核实）——✅ 已全部修复（2026-10-02）

> 修复纪要：H1 = `strip_jsonc_comments` 改按字符遍历 + 多字节回归钉
> （config/mod.rs `strip_comments_preserves_multibyte`）；H2 = ps≥2 分支防护 +
> `record_phrase_page_size_one` 钉；H3 = `route_key`/`chinese_punct_pending` 加
> `commit_punct_state`（Test 纯判定、Down 提交翻转）+ 复位/初值 false→true 纠正
> + mode.rs 4 个对称性钉；H4 = SetMode 兜底补 `english_mode.store` + 引号复位
> （与 apply_openclose 对称）；H5 = `Engine::clear_user_dict`（iuv-core）+
> engine 句柄传入 SettingsApp，「清除全部」同步重置内存态 +
> `clear_user_dict_resets_engine_memory` 钉。附带清掉 clippy 唯一警告
> （remote_host.rs clone_on_copy）。

| # | 位置 | 问题 |
|---|------|------|
| H1 | `crates/iuv-core/src/config/io.rs:134` | `strip_jsonc_comments` 按字节 `out.push(c as char)`，非 ASCII 字节被按 Latin-1 重组成错误 UTF-8——**配置值里的中文被静默改写**。测试只覆盖 ASCII（config/mod.rs:276-285），正好藏住。修法：按 `char_indices`/解码后的 char 遍历，补多字节用例。 |
| H2 | `crates/iuv-core/src/userdict.rs:212` | `entries[ps - 2]` 在 `page_size=1`（引擎只钳 `max(1)`，合法配置，engine.rs:36）时 **usize 下溢 panic**。修法：`n >= ps` 分支防 `ps < 2`；补 `page_size=1` 测试钉（engine_session.rs 现全默认 5）。 |
| H3 | `iuv-tsf/src/com/key_routing.rs:82` + `mode.rs:128-155` | **中文引号配对失效**：M10「Test 阶段真正处理」后 `route_key` 在 Test/Down 各跑一次，`chinese_punct_pending` 内 `punct_quote_open` 一次按键净翻转两次回原值→会话外引号恒开形、闭形不可达；只发 Down 的应用行为还不一致。附带：复位值 `false`（text_service.rs:208、mode.rs:52）按 punct.rs:36 语义（true=开形）实为复位到**关形**。修法方向：Test 阶段走无副作用的纯判定，翻转只留在 Down；或判定与翻转分离。 |
| H4 | `iuv-tsf/src/com/text_service.rs:265-273` | 工具栏 SetMode 写 OPENCLOSE 失败的兜底只改 `runtime.mode`，**漏同步 `english_mode` 原子量**（apply_openclose 两者都改）→ 按键路由（key_routing.rs:72）与语言栏图标读旧值，中英状态分裂。修法：兜底分支复用 apply_openclose 或补 `english_mode.store`。 |
| H5 | `iuv-server/src/daemon/settings.rs:1197-1206` | 设置页「清除全部」只清 `DaemonState.dict` 并写盘，**引擎内存态用户库不重载**；下一条 UserMutation 基于旧库写盘→被清条目复活。根因是用户库双真相源（引擎 vs DaemonState，同步只靠设置页打开时从磁盘重载 settings.rs:1109）。修法：clear 时同步重置引擎侧（engine attach 的 user dict），长期应收敛单真相源。 |

## 2. 中危精选（代理发现，修复前先复核）

### 2.1 transport/协议层（长连接稳定性根子）——✅ 前五项已修（2026-10-02）

> 修复纪要：T1 = `with_start` 步长恒 2 + 奇数段回归钉；T2 = request 超时清
> inflight 表项（烧号保留，语义注释化：单连接 ≤32768 次超时后 alloc=None →
> 调用方降级重连）；T3 = client Shared 加 `writers` 计数（request/ctl 应答写
> 均持 WriteGuard），Drop 收尾等写归零再放读线程关句柄，对齐 server 收尾协议；
> T4 = ConnSender.ids 改 `Arc<Mutex<StreamIdAlloc>>` 共享分配器（clone 不再
> 分叉出相同奇数号）；T5 = shutdown 只 CancelIoEx 不 CloseHandle + accept 线程
> ConnectNamedPipe 改 200ms 分片等待查 stop，句柄由 accept 线程统一单次关闭。

- `iuv-win/src/transport/client.rs:97-123` — 客户端 Drop 只护读线程，**并发写无 writers 计数**（服务端 server.rs:491 有对称防护）→ 句柄值复用窗口期可能写进别人的连接。
- `client.rs:268-279` — 请求超时**不释放 stream_id**、不清 inflight 表项 → 长连接永久烧号，32768 上限后客户端不可用。
- `crates/iuv-proto/src/stream.rs:37-43` — `with_start(odd=true)` 给 step=1，奇数段分配跨入偶数段，与文档矛盾；测试只覆盖 odd=false（proto.rs:463）。
- `iuv-win/src/transport/server.rs:94-101` — `ConnSender::clone` 快照复制 in_flight 分配器，两副本会分到相同序号（stream.rs:9 注释还背书了这个 Clone 语义）。
- `server.rs:228-301` — `stop()` 对 accept 中断句柄**双重 CloseHandle**（CancelIoEx 中断后被误判 connected）。
- `iuv-win/src/shm.rs:277-303` — seqlock 声明不成立：读侧无 version 复核，可能读到「新 data_len + 被覆写的数据」。
- `iuv-proto/src/frame.rs:67` — 帧头 urgent bit 声称优先出队，实际无优先级队列，服务端只回显——文档承诺超前实现。

### 2.2 server 迁移收尾

- ~~`src/lib.rs:129` — 重绑注册表只在带令牌重连时清扫~~（✅ 已修：sweep_resumes 每次 on_connect 调用）。
- ~~`src/daemon/toolbar/window.rs:755` — 工具栏四态翻转在 UI 线程**同步 dispatch 3s**~~（✅ 已修：分派改短命线程（server candwin 点击选词同款先例），结果写实例表 + WM_APP_REFRESH 跨线程唤醒重绘；HWND 经 usize 过线程对齐 ToolbarHost::wake 惯例）。
- ~~`src/daemon/log.rs:54` — `install_panic_hook` 定义后全仓无调用~~（✅ 已修：main.rs 安装）。
- ~~`src/main.rs:24` vs `src/daemon/log.rs:10` — 两套日志装配、设置页清错文件~~（✅ 已修：clear_logs 目标改 iuv-server.log，模块头注释归一）。
- `src/daemon/settings.rs:271-284` — dev 页 LOG_MODULES 的 tag 是 daemon 时代清单，与 server 实际 tag（`[toolbar]`/`[config]`…）大面积错位。
- ~~`src/lib.rs:441` — C2S catch-all 不回应答~~（✅ 已修：回 `S2C::Err(ProtoError::Unsupported)`，新变体追加枚举末尾）。

### 2.3 tsf COM

> 修复纪要（2026-10-02，工作区待真机回归）：Activate 改显式失败处理，中途失败回滚
> 已完成的 AdviseKeyEventSink；deactivate 活动会话走 flush_session（原文上屏 +
> EndSession + 清 last_effect），服务端会话不再残留、重激活首键不再被吞；
> remote_host 增 `REVIVE_REQUESTED` 挂号——reviving 闸被占时 Activate 兜底/离线
> 持续按键不再被丢弃，窗口失败由 `finish_revive` 代跑（带 spawn），焦点不变进程
> 不再永久透明；两份重连循环归一 `spawn_revive_loop`，`Proto`（版本/令牌拒绝）
> 提前放弃、瞬时不可达才按窗重试（重试不再无差别）。四条路径均无头测试不可达，
> 留真机回归验证。

- `src/langbar.rs:13` — 文件头声称「全部 COM 回调经 guard 包装捕获 panic」，**实际一个都没包**（langbar 回调、candwin.rs:429 / menu_window.rs:199 的 wnd_proc 均裸奔）；panic 穿透 `extern "system"` 拖垮宿主（text_service.rs:779 记载过 0xC0000409 事故）。guard 定义在 text_service.rs:53 是私有，应抽公共。（未列入本批修复，仍欠。）
- ~~`src/com/text_service.rs:357-367` — Activate 多处 `?` 中途失败无回滚已完成的 advise/attach~~（✅ 已修，见上纪要）。
- ~~`src/com/text_service.rs:495-510` — deactivate 直接清 session/composition，**不发 end_session 不清 last_effect**~~（✅ 已修，见上纪要）。
- ~~`src/com/remote_host.rs:539-576` — 延迟重连窗内 `reviving` 闸吞掉 Activate 兜底重生；焦点不变进程两次失败后再次永久透明~~（✅ 已修：挂号机制，见上纪要）。

### 2.4 横切

- `scripts/install.ps1:37-44` — Ensure-Imedic/Ensure-Opencc 在**提权窗口内跑 cargo**，违反 iuv-common.ps1:469 自家红线（提权进程丢 PATH）。
- `scripts/iuv-common.ps1:71-96` — Restart-Ctfmon 先杀 ctfmon 再注册计划任务，注册失败则系统留在无 ctfmon 态直到注销。
- `crates/iuv-proto/Cargo.toml:12`、`crates/iuv-data/Cargo.toml:9`、`iuv-server/Cargo.toml:17`（eframe）— 依赖声明绕过 workspace 集中管理。
- ~~`README.md:47` 写 Rust 1.85+，workspace 实际 rust-version=1.89~~（✅ 已修：README 改 1.89+）。

## 3. 死代码与文档漂移（迁移收尾症状，一批清）

### 3.1 死代码

- `crates/iuv-core/src/viterbi.rs` 整模块（143 行）+ `lm.rs:9 OOV_PENALTY` — 被 rime poet 取代后未拆，仅自测引用。
- `iuv-tsf/src/ui/candwin.rs:289-380、467-515` 本地候选窗约 600 行 — M10 服务端渲染下窗口永不创建；`render_locally` 全调用点恒传 false（session_bridge.rs:258、dispatch.rs:34），跳变检测 JUMP_THRESHOLD 整段生产不可达；`dispatch.rs:187` candidate_owner_apps 抑制逻辑作用在不可见窗上（**set_suppressed 实际失效**，候选抑制若要生效需移到 server 侧判定）。
- `iuv-server/src/daemon/state.rs:35 quit_flag`（主循环因此无退出路径）、`:28 close_settings`（关闭链路死）、`:91 bump_config_epoch`（shm 恒 None 空转，settings.rs:1187 调用是假「已广播」）、`daemon/toolbar/mod.rs:274 shutdown`、`TooltipWindow::show_near` 死参数。
- `iuv-proto/src/msg.rs` — `ProtoError::ServerGone`（无构造点）、`S2C::Ping`（无心跳实现）、`ClientResp(C2S)` 类型层放行全部变体但注释限定四个。
- 测试投入与死路径错配：candwin.rs 现有单测集中在已死的本地窗几何。

### 3.2 文档漂移（重点修三份，其余批量对账）

- ~~**`docs/plan/00-overview.md:28,34,68`** — 仍列 iuv-daemon 为活跃组件、旧数据流~~（✅ 已修：架构图/数据流改 M10 现状，Viterbi/M6 表述同步标注；**误导源之首**已拔）。
- ~~**`docs/knowledge/tsf-interaction.md:16,20`** — 机制规格仍是旧「ctl 管道 + 共享段」多套 IPC 形态~~（✅ 已修：进程模型/呈现通道/源码映射对齐 transport 长连接 + 服务端自绘，补变更记录）。
- ~~**`README.md:36` / AGENTS.md:71** — iuv-win 职责描述还是 M6 的「管道 IPC/共享段」~~（✅ 已修：两处职责行对齐 transport/ULW 现状；README 版本要求同步改 1.89+）。
- 代码内漂移（批量，部分已修）：~~`remote_host.rs:8-9`「每请求 20ms」实际 300ms~~、~~`Cargo.toml:4` 注释还提 iuv-daemon~~、~~`.gitignore` `/target-daemon` 死条目~~（✅ 均已修）；其余（`shm.rs:31-33` 句柄注释与 Drop 实现矛盾；`lib.rs:14-16`「UserMutation 空实现」实际已全量接线；`candwin.rs:14`「抑制命中不启动窗口线程」与 lib.rs:161 无条件 spawn 矛盾；`pet.rs:10-11` R/B 交换位置已变；`toolbar.rs:247` toolbar_size「已抽出复用」实际两处未调用；`text.rs/snapshot.rs` 仍引「iuv-tsf/src/...」旧路径）仍欠。
- 杂项：`docs/plan/48` 状态行「未提交」已过时；`docs/pet/GIRL-PET-SPEC.md:62-65` 四表情回退已过期（素材与映射均已落地）；`docs/issue/d冒号表现不一致.txt` 已闭环未归档。

## 4. 自动化检查明细

- **fmt**：88 处违例（import 排序、尾参数换行为主），`cargo fmt --all` 一把清。
- **clippy**：全仓 1 个警告（`remote_host.rs:258` clone_on_copy）。
- **测试**：约 42 个失败**全部是执行环境所致，非代码回归**——见 §6。
- **真测试 bug 1 个**：`iuv-win/tests/transport.rs:339` 管道名 `r"\.\pipe\..."` 少一个反斜杠（同文件 test_pipe():17 是对的），`server_initiated_request_roundtrip` 稳定失败「命名管道不可达: 3」。修法：改用 `test_pipe("ctl")`。
- **rtt_bench** 并发跑偶发、单独跑通过（时序敏感基准，考虑容忍或加重试）。

### 4.1 测试盲区汇总（修对应 bug 时顺手补）

- proto roundtrip 样本缺 8 个新变体（InstanceDeactivated/PendingTextQuery/CandwinHide/OpenSettings/ToggleToolbar/ToolbarVisibleQuery/S2C::ToolbarVisible/S2C::PendingText）——**postcard 序号纪律恰恰没有回归保护**；`with_start(odd=true)` 零覆盖。
- tsf：`chinese_punct_pending` Test/Down 双跑对称性、`merge_outcome` 增量合成、`apply_ctl_cmd` 各分支（正是 H4 藏身处）零测试。
- server：toolbar `apply_event`/`should_show`/`unbind_instance` 三维显隐、`awaiting_caret` 竞态、`validate_combo/find_conflict` 未测。
- ui：hover/pressed 视觉路径零覆盖；draw/measure 对齐承诺无直接断言；宠物呼吸偏移 mask 失配无回归钉。
- transport：无断连收尾/部分读写分帧/Drop 与在途写并发测试（P5 帧错乱修复无回归测试）。

## 5. 建议推进顺序

1. **清环境**（见 §6）：本机终端确认 42 失败归属；`cargo fmt --all`；修 transport.rs:339。
2. **修 5 个高危 H1-H5**（H1/H3/H4 小改动高收益；H2 补钳制+测试；H5 理顺双真相源），每个带回归钉。
3. **transport 补强**（§2.1）：客户端写保护 → stream_id 泄漏 → odd 分支 → ConnSender clone → stop() 双关。
4. **迁移收尾对账**（§3）：死代码清扫 + 「过渡期注释 vs 实际行为」对账 + panic hook 安装 + 日志装配归一。
5. 更新 00-overview / tsf-interaction / README 三份文档后再开新特性。

## 6. ⚠️ 执行环境发现：工作区进程围栏（42 个测试失败的真相，已破案）

**机制（2026-10-02 实锤）**：本机存在一个**按镜像路径生效的进程围栏**——
可执行文件位于 `D:\Projects\input` 下时，进程被创建为 **Low 完整性级别**
（同一 `whoami.exe`：System32 镜像 = Medium IL `S-1-16-8192`，拷入仓库树 = Low IL
`S-1-16-4096`）。Low IL 进程对 Medium 资源无写权 → **树外文件写与内核命名对象
创建（如 `Local\iuv-userdict-shm`）全部被拒**，仓库树内可写；**管理员令牌
（High IL）不受影响**。与杀软注册表、受控文件夹访问（已确认关闭）、SRP/AppLocker
（无规则）、fltmc 过滤列表（全微软组件）均无关；`D:\Projects\input` 目录上留有
非继承的显式 ACL（`Everyone:(DENY)(S,DC)` + 两个 `S-1-4-*` 怪 SID 的
`(OI)(CI)(W,D,DC)`），是安装此围栏的软件所种。元凶身份未定（运行中的第三方
驱动/服务无明确候选；存在 `ps_service`「Streaming Service」等待排查项）。

**影响与解法（已验证）**：
- 非提权测试 exe（永远住在 `target\debug\deps`，镜像在树内）写 `%TEMP%` 全拒 →
  42 个测试红。**解法：把 TMP/TEMP 指到树内再跑**：
  ```
  set TMP=D:\Projects\input\iuvim\target\tmp && set TEMP=D:\Projects\input\iuvim\target\tmp && cargo test --workspace --no-fail-fast
  ```
  实测 **518 通过 / 3 失败**（.gitignore 已含 target/，临时目录落树内无副作用）。
- 剩余 3 个失败 = `iuv-win` shm 三测试（内核命名对象 `Local\`，无路径可重定向），
  **需管理员终端跑**（High IL 免疫）：`cargo test -p iuv-win --lib`。
- 注意：不要常态化用管理员跑全量测试——target/ 里会混入管理员属主的产物，
  之后普通终端增量编译会报 Access denied；如已发生，`cargo clean` 可解。

**归类纠偏**：transport 的 `server_initiated_request_roundtrip` 与此围栏**无关**
（同环境其余 transport 测试全绿、报错「管道不可达」而非 os error 5），真因是
transport.rs:339 管道名少一个反斜杠——已修复，transport 套件 8/8 全绿。
