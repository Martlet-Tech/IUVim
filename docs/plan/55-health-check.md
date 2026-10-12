# 55 · 全仓库健康体检（M10 后第二轮）

> 状态：**体检报告（2026-10-12）**——报告只体检；清账进度见下与 `docs/status.md` 末条。
> **进度**：①③⑤⑥② 已落地（`chore/55-health-fixes`）；④ 日志轮转已落地并真机通过（`feat/log-rotate`，
> 真机发现开机标识 ±1s 抖动已回补容差）；⑦ 54 号第一步（清除自有 pet）已落地并真机通过（`feat/pet-removal`）。
> 基线：main @ `1867503`。上一轮 = 50 号全仓品质检查（`docs/closed/50-quality-audit.md`，2026-10-02）。
> 本轮定位：在 50 号基础上复检，并覆盖此后新增的桌宠 / 工具栏 / M3 相关代码。
> 方法：`cargo fmt/clippy/test` 全仓自动化 + 契约/依赖两路子代理逐文件审计 +
> 真机日志与部署核对 + pet 代码分层清点。

## 0. 结论摘要

工程基线整体健康：**测试全绿（仅 3 条 shm 需管理员）、无 panic、无 dead_code 告警、依赖无越出全局白名单**。
但有若干真实欠账，按严重度排列：

| 级别 | 项 | 位置 |
|---|---|---|
| 中 | 发布红线未消：install 在**提权窗口内跑 cargo** 编译词库 | `scripts/install.ps1:37-44` |
| 中 | 契约文档**大面积漂移**（最大误导源，50 号未修契约） | `docs/plan/01-contract.md`（20+ 处） |
| 中 | 日志无轮转/无上限，`iuv-tsf.log` 已 **50MB** | `platforms/windows/iuv-win/src/logger.rs`（机制） |
| 低 | fmt 脏（4 文件）、clippy 3 告警 | 见 §1 |
| 低 | 依赖越权 4 处、server rust-version 声明低于 eframe 要求 | 见 §4 |
| 低 | 测试盲区（proto 新变体 / tsf punct 对称 / server toolbar）仍未清 | 见 §5 |
| 低 | ITfSource QI 失败降噪疑似未生效（同 pid 多条） | `iuv-tsf` follow 日志 |

## 1. A · 自动化基线

- **fmt**：❌ 4 个文件有违例（未赶上 9-30 的 fmt 清扫）
  - `crates/iuv-core/examples/onkey_bench.rs:35,92,106`（53 号新增示例）
  - `crates/iuv-core/src/rime/mod.rs:656`
  - `crates/iuv-data/src/dict.rs:891,1490,1497`
  - `crates/iuv-ui/src/toolbar.rs:166,424`
  - 修法：`cargo fmt --all` 一把清。
- **clippy**：❌ 3 条告警（`-D warnings` 下直接编译失败）
  - `crates/iuv-data/src/dict.rs:744,745` — `doc_lazy_continuation`（clippy 1.96 新 lint，文档注释续行未缩进）
  - `crates/iuv-core/examples/onkey_bench.rs:131` — `unnecessary_sort_by`（应改 `sort_by_key(|b| Reverse(b.2))`）
- **check**：✅ 编译通过、无 warning。
- **test**：✅ 实测 **~532 通过 / 0 失败**；仅 `iuv-win` 3 条 shm 测试失败——
  `shm_config_epoch_bumps` / `shm_version_bumps_on_write` / `shm_write_read_roundtrip`，
  报 `创建共享段失败 Local\iuv-userdict-shm: 0x80070005`。**与 50 号 §6 一致，属"内核命名对象不可重定向"的环境限制，需管理员终端跑**，非代码回归。
  - 注意：必须 `set TMP/TEMP` 指到树内（`target\tmp`）规避"进程围栏"，否则 40+ 测试因写 `%TEMP%` 被拒而红。

> **本轮环境新发现**：在**受沙箱限制的自动化终端**里跑 `cargo build --release` 会稳定失败——
> proc-macro2 的 build script 清理 probe 目录报 `os error 145（目录不是空的）`。
> 免沙箱/普通窗口下同一命令立即成功（用户 2026-10-12 18:56 已正常 release 构建）。
> **结论：这是执行环境（沙箱）限制，非项目缺陷**，记录以免误导后续体检。

## 2. B · 契约与文档对账

**`docs/plan/01-contract.md` = 最大漂移源**（50 号只修了 00-overview/tsf-interaction/README，漏了契约）：

- L10 — 根路径写成 `D:\Projects\vaim\` → 实际 `d:\Projects\input\iuvim\`
- L18–19 — `scripts\register.ps1` / `unregister.ps1` → 实际 `install.ps1` / `uninstall.ps1`
- L21–62（§1 骨架图）— crate 仅列 iuv-data/iuv-core/iuv-repl、windows 仅 iuv-tsf → 缺 iuv-proto/iuv-ui/iuv-win/iuv-server；L48 "iuv-tsf + GDI 候选窗"已退役
- L38–39 / L400–423 / L426 / L552 — §4 的 `rerank.rs`（RerankStage/StaticOrder）与 `store.rs`（UserDataStore/NullStore）**文件已删除**；`Engine{stages, store}` 现为 rime 引擎
- L60 / L646 — `iuv-tsf\src\ui\candwin.rs` → 实际 `platforms/windows/iuv-server/src/candwin.rs`
- L61 — `examples\candwin_demo.rs` 已删除
- L92–100 — windows features 表缺 `Win32_Security_Cryptography`、`Win32_System_SystemInformation`
- L119 — iuv-data 依赖仅书 `serde` → 实际另有 `windows-core`、`windows`
- L123 — iuv-tsf 依赖缺 `iuv-proto`、`iuv-win`
- L124 — `iuv-daemon` 依赖行 → crate 已删（并入 iuv-server）
- L641 — `iuv-core/src/{schema,lm,viterbi,rerank,store,engine,session}.rs` → viterbi/rerank/store 已删
- L644 — `iuv-tsf/src/ui/gdi.rs` 已删
- L648 — `iuv-data/src/{shm,ipc}.rs` → 实际 `iuv-win/src/shm.rs` + `iuv-win/src/ipc/`
- L649 — `iuv-tsf/src/daemon_client.rs` → 实际 `com/daemon_host.rs`、`com/remote_host.rs`
- L650 — `scripts/{register,unregister}.ps1` → install/uninstall
- L670 — "引擎在 DLL 内 `OnceLock<Arc<Engine>>`" → M10 后引擎在 iuv-server（薄客户端）
- 自相矛盾：同文件 §4（旧 rerank/store）与 §8（rime 管线）并存

**其他：**
- `docs/plan/00-overview.md:28,29` — iuv-tsf/iuv-win 依赖行缺 iuv-proto/iuv-ui/iuv-core 等
- `README.md:16` — 链接指向 `docs/plan/39-rime-pipeline.md` → 实际在 `docs/closed/39-...`
- `AGENTS.md` / `docs/status.md` / `docs/knowledge/tsf-interaction.md` — **未见可证实漂移**（tsf-interaction 已对账 M10）

## 3. C · 死代码 / 语义漂移

- **iuv-core 语义漂移仍在**：`pub mod pet_model / pet_physics / pet_skin`（`crates/iuv-core/src/lib.rs:10-12`）——
  "引擎"crate 装桌宠逻辑。与 54 号第一步（清除自有 pet）重叠，见 §9。
- `allow(dead_code)` / `todo!()` / `unimplemented!()` / `FIXME` / `TODO` — 全仓仅 **6 处**，无成堆死代码。
- clippy 未报 `dead_code` 告警 → 无整块死模块（对比 50 号当时的 viterbi / tsf 本地候选窗）。

## 4. D · 依赖与合规

**越权依赖（子 crate 直写版本，未走 `[workspace.dependencies]`）：**
- `crates/iuv-proto/Cargo.toml:11` — `postcard = { version = "1", ... }`
- `crates/iuv-data/Cargo.toml:10` — `windows-core = "0.62"`（根已有同名 workspace 项）
- `platforms/windows/iuv-tsf/Cargo.toml:24` — `winres = "0.1"`（build-dep）
- `platforms/windows/iuv-server/Cargo.toml:19` — `eframe = "0.36"`

**白名单对照（契约 §2.2）：** 无**全新**第三方 crate 越出全局白名单；但存在**逐 crate 范围超限**——
- `iuv-data`（该行仅允许 serde）实际用了 `windows-core` / `windows`
- `iuv-server`（daemon/server 行）实际用了 `tiny-skia`
- `platforms/windows/iuv-win` **整 crate 不在 §2.2 表内**（表内仍是已退役的 iuv-daemon → 文档滞后）

**版本一致性：**
- 8 个子 crate 的 edition/rust-version/license 均 `.workspace = true` → 一致 ✅
- ⚠️ 隐患：iuv-server 依赖 `eframe 0.36`（契约注其 `rust-version` 单独要求 1.95），
  但 `platforms/windows/iuv-server/Cargo.toml:5` 仍 `rust-version.workspace`（1.89）——**声明低于依赖实际要求**。

**许可与 NOTICE：**
- 仓库**无顶层 LICENSE / NOTICE / COPYING**；唯一许可文件 `assets/pet/LICENSE.md`（桌宠素材 CC0）
- 白霜拼音 GPL-3.0、OpenCC Apache-2.0 仅有**文字声明**（README:71 / AGENTS:74 / 02-conventions:46-49 等），**无 NOTICE 文件落实**——发布包若含编译产物需补。

## 5. E · 测试盲区（50 号 §4.1 核销）

以下 50 号点名的盲区**仍开放**：
- `crates/iuv-proto/tests/proto.rs` — **零覆盖**新变体（CandwinHide / OpenSettings / ToggleToolbar / ToolbarVisibleQuery / S2C::ToolbarVisible / PendingTextQuery / S2C::PendingText / InstanceDeactivated）；
  即 **postcard 序号纪律没有回归保护**。
- `iuv-tsf` `chinese_punct_pending` 的 **Test/Down 双跑对称性**无测试（`mode.rs:132` 仅实现，无测试）。这是 H3 当年的藏身处。
- `iuv-server` toolbar 的 `apply_event` / `should_show` / `unbind_instance`（`window.rs:290,988,269`）无直接测试。

## 6. F · 运行时 / 真机体检

- **无 panic**（tsf/server 日志均无）。
- ✋ **`iuv-tsf.log` 已 50.5 MB 且无轮转/无大小上限**（server 日志仅 687KB）。日志长期只增不减，需评估轮转或按模块截断策略（注意硬约束：日志失败不得影响输入）。
- ITfSource QI 失败（`0x80004002`）——文档称"每进程首条 + 计数"降噪，但实测**同一 pid（Trae CN 13212）多条重复**，降噪疑似未按预期生效。低优先复核（Electron 宿主恒失败属不支持布局 sink，本身非缺陷）。
- 部署态：三产物 + 词库哈希与源一致、注册表双架构路径正确、`iuv-server`/`ctfmon` 运行中；残留 `.old` 2 处 + `PendingFileRenameOperations` 条目（下次重启清理）。

## 7. G · 性能基线（release，1,254,086 词条）

引擎单键耗时（`onkey_bench`，page_size=5）：

| 输入串 | 键数 | 总值 | 均值 | 最大 |
|---|---|---|---|---|
| ni | 2 | 648.5µs | 324µs | 384µs |
| nihao | 5 | 2.0ms | 400µs | 1.05ms |
| jintiantianqizhenbucuo | 22 | 45.6ms | 2.07ms | 4.24ms |
| zhonghuarenmingongheguo | 23 | 31.2ms | 1.36ms | 3.51ms |
| woshiyigechengxuyuan | 20 | 18.5ms | 925µs | 3.40ms |
| xianzaiyoumeiyoukaqi | 20 | 35.1ms | 1.75ms | 4.02ms |
| wangshangliulanqi | 17 | 54.2ms | 3.19ms | 12.27ms（慢键 h@5） |
| chifanhema | 10 | 7.8ms | 780µs | 2.71ms |

- 词库加载 **22.42ms**。
- 说明：这是**引擎层**基准，非端到端；服务端另有 `[perf]` ≥10ms 观测线（见 status.md）。

## 8. H · 发布 / 安装链路

- ❌ **`scripts/install.ps1:37-44` 仍在提权窗口内调用 `Ensure-Imedic/Ensure-Opencc`**（会跑 `cargo run -p iuv-data --bin dictc`），
  违反 `iuv-common.ps1:468` 自家红线"提权进程丢 PATH，cargo 会闪退"——50 号 §2.4 点名，**至今未修**。仅在全新机器（词库缺失需现编）时才暴露。
- `scripts/iuv-common.ps1:71-96` `Restart-Ctfmon` 先杀 ctfmon 再注册计划任务，注册失败则系统停在无 ctfmon 态（50 号已记，仍在）。

## 9. 附 · 54 号第一步「清除自有 pet」清理清单

> 用户已定：54 号分两步——先**清除自有 pet**（解耦），再做**附着**。本清单为第一步施工图。

**pet 专属源文件（5 个，共 3,081 行）：**

| 行数 | 文件 |
|---|---|
| 644 | `crates/iuv-core/src/pet_model.rs` |
| 648 | `crates/iuv-core/src/pet_physics.rs` |
| 380 | `crates/iuv-core/src/pet_skin.rs` |
| 1118 | `crates/iuv-ui/src/pet.rs` |
| 291 | `platforms/windows/iuv-server/src/daemon/pet_assets.rs` |

**依赖它们的文件（按层）：**
- **iuv-core**：`src/lib.rs:10-12`（模块声明）、`:33-35`（re-export）— 需摘除
- **iuv-ui**：`src/lib.rs:16,27-29,36-37`（模块 + re-export）、`src/toolbar.rs`（`PET_OVERHANG/PET_DISPLAY_W/H`、`PetRenderSpec/LayeredPetSpec/PetSpec`、`pet_display_rect`、`render_composite` 的宠物区与 mask；含 208–391 段与多处测试）
- **iuv-server**：`daemon/toolbar/mod.rs`（`pet_art` 注入）、`daemon/toolbar/window.rs`（`pet_model/pet_anim/pet_mask/pet_timer/pet_last_pos/pet_rand/pet_down`、`PET_TIMER_ID/PET_IMPULSE_PER_PX/PET_CLICK_IMPULSE/PET_DRAG_THRESHOLD/PET_HIT_ALPHA`、`sync_pet_timer`、on_ime_state/on_typing 驱动）、`daemon/pet_assets.rs`、`daemon/settings.rs`、`daemon/mod.rs`、`main.rs`
- **iuv-proto**：`src/msg.rs:46`（`Caps::PET`）、`:334`（打字活动注释）；`tests/proto.rs:130`
- **iuv-tsf**：`src/com/dispatch.rs`（pet 相关分支）

**解耦关键**：宠物区目前**长在工具栏 HWND 内**（栖木式，overhang=136），54 号要求让工具栏成为**独立宿主**、宠物区降级为**可选**；`PetModel/PetAnim` 等的归属需从 iuv-core 迁出（同时消化 §3 的语义漂移）。

## 10. 建议推进顺序

1. **清基线**（低风险快赢）：`cargo fmt --all` + 修 3 条 clippy 告警。→ 让后续改动都有干净基线。
2. **修发布红线**：`install.ps1` 把词库编译移出提权窗口（对齐 dev-deploy 的"本窗口构建→提权只部署"）。
3. **契约文档对账**：重写 `01-contract.md` 现状（crate 清单 / 文件映射 / rime 管线 / 依赖表），清 `00-overview` 与 README 链接。
4. **日志轮转**：给 `iuv-win::logger` 加上限/轮转（守住"日志不影响输入"硬约束）。
5. **依赖收敛**：4 处越权依赖归并到根 workspace；修正 iuv-server rust-version；补 NOTICE。
6. **测试补盲**：proto 新变体 roundtrip 优先（序号纪律无保护）。
7. **54 号第一步**：按 §9 清单清除自有 pet + 工具栏解耦（顺带清 iuv-core 语义漂移）。
8. 54 号第二步（附着）落地后，针对新架构再做一次收尾小体检。