# 61 · 发布线第一周：CI + 安装包 + v0.2.0 + server 崩溃自愈

> 状态：**进行中**（2026-10-20 立项，分支 `feat/release-0.2`）。
> 源起：2026-10-20 战略复盘——项目 10 周 295 提交、443 个测试、32k 行 Rust，
> 但**无 CI、无 tag、无安装包、无外部用户**。「能不能做出来」已回答完，
> 接下来要回答「别人愿不愿意用」。本任务书 = 后者的第一步：把仓库变成**可交付物**。
>
> 前置阅读：`02-conventions.md`（构建/测试纪律）、`49-thin-client-arch.md`（server 单点架构，§4.5 失效语义）、
> `50-quality-audit.md` §6（本机进程围栏与测试环境注意事项）。

## 1. 目标（验收一句话）

**在一台干净的 Win10/11 x64 机器上，双击一个安装包，重启一次，就能用 iuv 打中文；
且此后每次 `git push`，CI 自动跑 fmt + clippy + test 并给绿/红。**

三条可独立验收的判据：

| # | 判据 | 验收方式 |
|---|---|---|
| A | CI 在 push 到 `main` 与 PR 时自动跑 fmt/clippy/test，结果可见 | GitHub Actions 页面出现绿勾；故意推一个 fmt 错误 → 变红 |
| B | `v0.2.0` tag 存在，且 Release 附一个可下载的安装包 | 从 Release 下载 exe → 干净机器安装 → 打字成功 |
| C | `iuv-server` 崩溃后自动重启，用户无需注销/切焦点 | 任务管理器结束 iuv-server → ≤10s 内自动回来 → 继续打字成功 |

## 2. 现状（已核实 2026-10-20）

| 位置 | 现状 |
|---|---|
| 仓库根 | **无 `.github/`**，零 CI。`cargo test --workspace` 只能人肉跑 |
| 版本号 | 8 个 crate 全 `0.1.0`；`iuv-tsf/build.rs:37-38` 硬编码 `0.1.0.0`；**无 git tag** |
| 安装 | 只有 `scripts\install.ps1`（开发脚本，需 Rust 环境 + 管理员 + 手动跑） |
| 自启 | `Deploy-IuvServer` 注册计划任务 `Iuv-ServerStart`（`AtLogOn` + `RunLevel Limited` + 无限时限）——**只保证登录时起，进程崩溃后无人拉起** |
| 客户端兜底 | `remote_host.rs` 已有三层：首连失败拉起（`start_remote_load`）、请求失败后台重生（失效语义 C，`schedule_revive`）、优雅停机延迟重连。**但都靠"按键/Activate 触发"**——空闲时 server 挂了没人管，用户回来打字才恢复（1-3s 透明） |
| 测试基线 | 本机 `cargo test --workspace`：**443 过 / 0 败**（`rtt_bench` 单跑必过、与全量并行时曾因管道竞争失败一次——见 §5-①②） |
| 工具链 | rustc 1.96.0 msvc；已装 `i686-pc-windows-msvc` target；`gh` 已装但**未登录**；`winget` 可用（Inno Setup 6.7.3 可装） |
| 本机特殊 | 本机存在按镜像路径的**进程围栏**（`50-quality-audit.md` §6）：在 `D:\Projects\input-method` 下创建的子进程被降至 Low 完整性 → `.git`/`target` 写被 ACL 拒。**这是本机环境问题，与仓库无关**；CI runner 不受影响 |

## 3. 设计

### 3.1 CI（`.github/workflows/ci.yml`）

**一个 workflow、三个 job、Windows runner**（唯一平台——TSF 是 Windows 专属，跨平台 crate 的
类型检查在 Windows 上一样做）。

```yaml
jobs:
  fmt:      cargo fmt --all -- --check
  clippy:   cargo clippy --workspace --all-targets -- -D warnings
  test:     cargo test --workspace --no-fail-fast
```

关键设计点（每条都有本机踩坑依据）：

1. **`IUV_SKIP_WINRES` 不置位**。TSF 的 `build.rs` 用 `winres` 调 `rc.exe` 编资源；GitHub
   Windows runner 自带 Windows SDK，正常编。逃生舱只留给"没有 SDK"的沙箱。
2. **不做 x86 车道**。CI 只跑默认 target（x64）；x86 TSF 由发布流程单独构建（见 §3.2）。
   理由：x86 腿慢一倍，且 x86 失败几乎只由依赖的 cfg 引起，本地发布时能兜住。
3. **shm 三测试**：`Local\` 命名对象在 GitHub runner 的交互会话里可创建（本机失败是本机
   低完整性所致，非代码问题）。**保留不跳过**——若 CI 真红，再按 §5 决定是加
   `#[ignore]` 还是改 `Global\`。
4. **`rtt_bench` 保留**。它是传输层唯一的性能护栏；并行全量测试时偶发管道竞争已在
   §5-① 记录，真复现则给它加 `--test-threads=1` 独立 job。
5. **缓存** `~/.cargo/registry` + `target`（`Swatinem/rust-cache@v2`），否则 Windows 上
   冷编译 5-8 分钟。
6. **超时 30 分钟**；`concurrency` 按分支取消旧跑。

### 3.2 安装包（Inno Setup 6）

**选 Inno 不选 MSI/NSIS**：单文件 exe、原生支持"装完跑一段 Pascal 脚本"、免费、
在 GitHub runner 上 `choco install innosetup` 一条命令。

新增文件：

```
installer/iuv-setup.iss          Inno 脚本（产物 + 注册 + 计划任务 + 卸载）
installer/README.md              本地怎么出包
scripts/package.ps1              一条命令：构建三产物 → 词库 → ISCC → dist\iuv-setup-0.2.0.exe
```

**装什么**：
- `iuv_tsf.dll`（x64）→ `{app}`，`regsvr32` 注册
- `iuv_tsf_x86.dll`（x86）→ `{app}`，SysWOW64 `regsvr32` 注册
- `iuv-server.exe` → `{app}`
- `iuv.imedic` + `iuv.opencc` → `%LOCALAPPDATA%\iuv`（**词库不进 Program Files**：
  36MB 词库属用户数据，卸载时保留，与现有 `Get-IuvPaths` 一致）
- 计划任务 `Iuv-ServerStart`（AtLogOn / Limited / 无限时限 / **带崩溃重启**，见 §3.3）

**不做什么**（本版明确排除）：
- 不做代码签名（无证书；Windows SmartScreen 会拦一次，文档里写清楚"更多信息 → 仍要运行"）
- 不做自动更新
- 不做静默安装参数以外的 CLI（`/SILENT` 由 Inno 自带）

**卸载**：Inno 的 `UninstallRun` 调 `scripts\uninstall.ps1`（它已经做全了：停服务、删注册表、
备份用户数据、延迟清理）。安装包自带该脚本副本。

### 3.3 server 崩溃自愈（本任务唯一真代码改动）

**问题定性**：薄客户端架构下 server 是全系统单点。现状 `Iuv-ServerStart` 只 `AtLogOn`，
进程退出后**系统层面无人重启**；客户端的 `schedule_revive` 只在"有按键"时触发。

**方案（实测定稿，2026-10-20）**：

**第一层｜计划任务的「周期触发器 + IgnoreNew」（采用）**

~~原打算用 `RestartCount/RestartInterval`~~ → **实测评证**。本机六轮探针：

| 探针 | 配置 | 结果 |
|---|---|---|
| 1 | `-RestartCount 3 -RestartInterval 1min`，手动 `/run` | 只跑 1 次，不重启 |
| 2 | 同上 + 真实 Once 触发器 | 只跑 1 次，不重启 |
| 3 | 同上，动作跑 3 秒后 exit 1 | 只跑 1 次，不重启 |
| 4 | 同上，动作写文件计数 | 1 分钟内只 1 次 |
| 5 | 一次性触发 + 每 1 分钟重复 | **2 分钟内 3 次** ✅ |
| 6 | 登录触发 + 每 1 分钟重复 | **运行期间每分钟尝试一次，IgnoreNew 生效** ✅ |

`<RestartOnFailure>` 写进了任务 XML（`schtasks /query /xml` 可见），但任务**从不被重启**。
官方文档也只在"task fails for any reason"上含糊其辞 —— 见
[RestartOnFailure (settingsType) Element](https://learn.microsoft.com/windows/win32/taskscheduler/taskschedulerschema-restartonfailure-settingstype-element)。

**采用方案**：两个触发器（`AtLogOn` + `Once` 带 `RepetitionInterval=1min`、
`RepetitionDuration=3650 天`）+ `MultipleInstances=IgnoreNew`。

- server 活着 → 周期触发被 IgnoreNew 忽略（任务处于 Running，不会起第二个进程）；
- server 死了（崩溃/被强杀/正常退出都算，任务变成 Ready）→ 下一次触发（≤60s）拉起来。

**代价与配套**：server 被 TSF 客户端抢先拉起时，任务视角自己没在跑，周期触发会真起一个
进程 → 靠 server 的**单实例守卫**立即退出。因此把守卫从 `main()` 中部**提到一切副作用
之前**（`main.rs:26-46`）：否则每分钟会重复发布 boot id 指针（与 TSF 读指针竞争）、
写一行噪声日志、并 mmap 一次 36MB 词库只为退出。

**语义与本方案的差异**（相对原 RestartOnFailure 设想）：
- 正常停机（哨兵 `server.stop` → `exit 0`）后，下一分钟会被重新拉起。部署流程是
  「停 → 换文件 → 启动」，换文件在 1 分钟内完成，所以不会与旧进程打架；若部署超过
  1 分钟，可能拉起**旧 exe**（它仍持有旧文件），`Replace-InUseFile` 的改名替换机制
  本就为此设计（旧进程持 `.old` 映射），不是新风险。
- 词库缺失（`exit 1`）会**每分钟**重试，不会放弃。这是刻意选择：日志里每秒重复 vs
  用户修好词库后自动恢复，后者更重要。噪声由 `[perf]` 之外的启动日志承担，可接受。

**第二层｜server 侧看门狗守护（不做）**
写一个 `iuv-watchdog.exe` 常驻监控。**否决理由**：多一个进程 = 多一个故障面，
且计划任务已覆盖。留作后续议题。

**第三层｜客户端空闲探测（不做）**
TSF 侧加空闲定时器要在每个宿主进程里跑一个定时线程，成本 > 收益（用户下次按键
1-3s 内自恢复，且不丢输入）。**计划任务周期拉起是正解。**
→ 这条要写进 `docs/status.md`，防以后反复重推。

**因此本任务书 §3.3 实际改动 = 计划任务触发器/设置 + `main.rs` 守卫前移 +
部署脚本同步**（`iuv-common.ps1` 的 `Deploy-IuvServer`、`scripts/install-steps.ps1`
两处，保持一致）。

### 3.4 版本号归一

- `Cargo.toml` 的 `[workspace.package]` 加 `version = "0.2.0"`，8 个 crate 改
  `version.workspace = true`
- `iuv-tsf/build.rs` 的 `FileVersion`/`ProductVersion` 改成**从 `CARGO_PKG_VERSION` 读**
  （现在是硬编码，每次发版必漏）
- `git tag v0.2.0` + GitHub Release（附安装包 + 从 `docs/status.md` 摘一段 changelog）

## 4. 实施拆分（按提交粒度）

| # | 提交 | 内容 | 验收 | 状态 |
|---|---|---|---|---|
| 1 | `docs(61)` | 本任务书 | 文档入库 | ✅ |
| 2 | `chore(release)` | 版本号 0.2.0 归一 + build.rs 读 `CARGO_PKG_VERSION` | DLL 属性里版本 0.2.0.0 | ✅ 实测两个 TSF 产物均 0.2.0.0 |
| 3 | `ci` | `.github/workflows/ci.yml`（fmt / clippy / test / rtt-bench 四 job） | push 后 Actions 绿 | ✅ 本机等价跑全绿 |
| 4 | `feat(server)` | 计划任务周期自愈 + `main.rs` 守卫前移 | 杀掉进程 → ≤1min 自动回来 | ✅ 探针验证触发器，待装包后真机复验 |
| 5 | `feat(installer)` | `installer/iuv-setup.iss` + `scripts/package.ps1` + `install-steps.ps1` | 本机跑出 `dist\iuv-setup-0.2.0.exe` | ✅ 18.3 MB 已出包 |
| 6 | `docs` | `installer/README.md` + `README.md` 加"安装"一节 + status.md | 干净机器能照文档装 | ✅ |

本机等价 CI 结果（2026-10-20）：
- `cargo fmt --all -- --check` → 0
- `cargo clippy --workspace --all-targets` → 0 告警
- `cargo test --workspace --no-fail-fast` → **443 过 / 0 败**（此前 `rtt_bench`
  并行失败一次，单跑与重跑全量均通过；已拆成 CI 独立 job 消除不确定性）

## 5. 已知风险与未决点

1. **`rtt_bench` 并行失败过一次**（本机全量跑：`per_request_connect` 的 P50 断言失败；
   单跑必过）。已按预案拆成 CI 独立 job（`--test-threads=1`），断言未动。
2. **shm 三测试在 CI 上的可行性**未验证（本机因低完整性失败，非代码问题）。
   GitHub runner 是交互会话，预期可过；**首次 CI 红要第一时间看这三条**。
3. **无代码签名** → SmartScreen 警告。本版接受，README 写清楚了。
4. **`{localappdata}` 跟提权账户走**：用另一个管理员账户提权时词库会落到那个账户，
   登录用户打字时"词库加载失败"。同账户提权正常。彻底解决（per-user 安装或挪
   `%ProgramData%`）留 v0.3。
5. **`gh` 未登录** → 本机**无法创建 Release**。需要用户 `gh auth login`，或手动在
   GitHub 页面传包。CI workflow 不需要（用仓库内建 `GITHUB_TOKEN`）。
6. **本机围栏**：本机跑 `cargo test` 需 `TMP` 重定向到 `target\tmp`；CI 不需要。
   README 已写明。
7. **计划任务每分钟起一次 server 进程**：靠单实例守卫立即退出。若未来降低触发频率，
   注意自愈窗口会同步变长。
8. **PowerShell 脚本必须 UTF-8 BOM**：仓库里 `iuv-common.ps1` / `uninstall.ps1` /
   `download-opencc.ps1` 原本无 BOM，Windows PowerShell 5.1 按 GBK 解读导致中文乱码
   甚至解析失败（本次已统一补 BOM）。新加 `.ps1` 一律带 BOM。

## 6. 不做（防扩张）

- ❌ 代码签名 / 自动更新 / MSI / winget 上架 —— v0.3.0 再议
- ❌ Linux/macOS 平台 —— 占位保持原样
- ❌ CI 里跑真机验收 —— 真机验收仍是人工，CI 只管机器可判定的部分
- ❌ 监控/遥测 —— 与"不记录输入内容"承诺冲突
- ❌ Inno `[Code]` 里手写计划任务 XML —— 会与 `iuv-common.ps1` 形成两份真相

## 7. 变更记录

- 2026-10-20 立项。CI + 安装包 + 崩溃自愈三线并行，目标 1 周。
- 2026-10-20 **§3.3 方案订正**：`RestartOnFailure` 六轮探针实测不触发 → 改
  「周期触发器 + IgnoreNew」；配套把 `main.rs` 单实例守卫提到一切副作用之前。
- 2026-10-20 出包成功（`dist\iuv-setup-0.2.0.exe`，18.3 MB），版本资源 0.2.0.0 实测确认。
- 2026-10-20 Inno `[Code]` 方案废弃：`TStringList.SaveToFile(..., TEncoding.Unicode)`
  在 Inno Pascal 里不存在（编译报错），且会与 `iuv-common.ps1` 双写计划任务。
  改为 `scripts/install-steps.ps1` 复用已验证的 PowerShell 逻辑。
