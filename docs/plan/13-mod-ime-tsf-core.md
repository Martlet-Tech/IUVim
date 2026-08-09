# 13 · 任务书 D：ime-tsf 管线（COM/TSF + 注册）

> 属主文件：`crates/ime-tsf/src/{lib,registration,log,session_bridge,composition}.rs`、`src/com/**`、`build.rs`、`scripts/{register,unregister}.ps1`、Cargo.toml 的 winres 配置
> 前置阅读：`00-overview.md`、`01-contract.md`（§5 接缝、§5.1 注册常量、§7 集成约定）、`30-conventions.md`
> **禁止**修改 `src/ui/mod.rs`（W0 冻结）。候选窗只通过 `CandidateUi` trait 使用，构造 `GdiCandidateWindow` 时调用 Agent E 提供的 `ui::gdi::GdiCandidateWindow::new() -> Self`（若 W1 期间 E 尚未完成，先用 `ui/mod.rs` 临时桩 `NullCandidateUi`——W0 已在 ui/mod.rs 提供该桩）。

## 1. 目标

产出可注册的 `input_ime_tsf.dll`：纯 Rust 实现 TSF 文本服务，把按键流接到 `ime_core::Session`，
把 `Effect` 应用到 composition 与候选窗。配注册/注销脚本。x64。

## 2. 参考实现（允许照抄结构）

- `RagibHasin/uo-keyboard`（GitHub，MPL-2.0）：纯 Rust TSF 的 COM 组织方式、`DllGetClassObject`、注册写法
- `microsoft/Windows-classic-samples` 的 `SampleIME`（C++）：TSF 调用时序语义
- 注意：参考其结构与 TSF 语义，**签名以本契约为准**。uo-keyboard 为 MPL-2.0，只可借鉴不可逐行搬运。

## 3. 交付清单与实现要点

### 3.1 `registration.rs`（常量已由 W0 写入）

- `DllRegisterServer`/`DllUnregisterServer`（`#[no_mangle] extern "system"`）：
  1. 用 `windows-registry` 写 `HKCR\CLSID\{C69735F1-...}`：`InprocServer32` = 本 DLL 路径（`GetModuleFileName`），`ThreadingModel="Apartment"`
  2. `CoCreateInstance(CLSID_TF_InputProcessorProfiles)` → `ITfInputProcessorProfiles::Register(CLSID)` +
     `AddLanguageProfile(LANGID_ZH_CN, PROFILE_GUID, PROFILE_DESCRIPTION, icon=dll,-1)` +
     `ITfCategoryMgr::RegisterCategory(clsid, GUID_TFCAT_TIP_KEYBOARD, clsid)`
  3. Unregister 反向。失败返回明确 HRESULT
- regsvr32 兼容：脚本直接 `regsvr32 /s input_ime_tsf.dll`

### 3.2 `lib.rs`：COM 导出

- `DllGetClassObject` 返回 class factory（`com/class_factory.rs`）
- `DllCanUnloadNow`：无活动对象时 S_OK（用全局对象计数）
- 不需要自写 DllMain

### 3.3 `com/text_service.rs`：接口实现

必实现（`windows::Win32::UI::TextServices`）：
- `ITfTextInputProcessor`（Activate/Deactivate）+ `ITfTextInputProcessorEx`
- `ITfKeyEventSink`（OnSetFocus/OnTestKeyDown/OnKeyDown/OnKeyUp…）
- `ITfThreadMgrEventSink`（OnInitDocumentMgr/OnSetFocus/OnKillFocus：焦点离开时 hide 候选窗并丢弃 Session）
- Activate 时：`AdviseKeyEventSink`；Deactivate 时 `UnadviseKeyEventSink` + hide

引擎单例：`static ENGINE: OnceLock<Arc<Engine>>`，路径 `%ProgramData%\InputIME\input.imedic`
（`SHGetKnownFolderPath(FOLDERID_ProgramData)` 或环境变量 `ProgramData` 拼接）。加载失败记日志，
进入"透明模式"：全部按键放行。

**Shift 临时英文**（必做，小功能）：Session 非 active 时按 Shift 切换 `english_mode: bool`（存在 TextService 实例上）；
english_mode 下 `OnTestKeyDown` 一律返回 FALSE。会话 active 时 Shift 不切换（放行给会话？MVP：直接忽略）。

### 3.4 `session_bridge.rs`：按键映射 + Effect 应用（契约 §7）

- vk → `Key`：`VK_A..VK_Z`→`Char(小写)`；`VK_OEM_7`(`'`)→`Char('\'')`；`VK_BACK/SPACE/RETURN/ESCAPE/PRIOR/NEXT/UP/DOWN`→对应；`VK_1..VK_9`（无 Shift）→`Digit(n)`
- `OnTestKeyDown` 规则：Session active → 上表内键一律吃掉；非 active → 仅字母键吃掉（开启会话），其余放行
- 应用 Effect：
  1. `composition.rs`：`SetText(effect.composition)`（无 composition 且有内容 → StartComposition）
  2. caret：`ITfContextView::GetTextExt(composition range, …)` → `CaretRect`（失败则用上一次位置，首次用屏幕中央）
  3. `ui.show/update`（`effect_to_snapshot`）
  4. `end`：`Commit(text)` → composition `SetText(text)` + EndComposition + ui.hide + 丢弃 Session；
     `Cancel` → `SetText("")` + EndComposition + ui.hide + 丢弃 Session

### 3.5 `composition.rs`

封装：`start(context)` / `set_text(range 替换)` / `end()`。要点：`RequestEditSession` 同步回调
（`TF_ES_SYNC | TF_ES_READWRITE`），edit session 内做 SetText；composition range 用
`ITfInsertAtSelection::InsertTextAtSelection(TF_IAS_QUERYONLY…)` 或 composition range 的 `SetText`。
按 uo-keyboard / SampleIME 的通行写法。`GUID_PROP_COMPOSING` 属性设置可选（MVP 可省，记事本等会自绘下划线）。

### 3.6 `log.rs`

`log_line(msg)`：追加写 `%TEMP%\input-ime-tsf.log`，带时间戳与进程名；全 crate 错误路径必记。
关键事件记日志：Activate、引擎加载结果、每次 commit、注册结果。

### 3.7 `build.rs` + 版本资源

`winres` 写入文件描述/版本/图标（图标可用默认空 ico，`res/` 下自带一个 1x1 占位）。

### 3.8 脚本

- `scripts/register.ps1`：复制 `target\release\input_ime_tsf.dll` 到 `%ProgramData%\InputIME\`（自建目录）→
  复制 `data\input.imedic` 同目录 → `regsvr32 /s` → `taskkill /f /im ctfmon.exe; start ctfmon` → 提示切输入法验证
- `scripts/unregister.ps1`：`regsvr32 /s /u` + 删文件 + 重启 ctfmon
- 两脚本检测管理员权限，非管理员给出明确报错

## 4. 测试与 DoD

TSF 难以自动测，DoD 为构建级 + W2 手测：

```
cargo check -p ime-tsf                 # 无 warning
cargo build -p ime-tsf --release       # 产出 input_ime_tsf.dll
```
- `ui/mod.rs` 桩集成：用 `NullCandidateUi` 也能编译链接（证明与 E 解耦）
- 代码内对 unsafe 块写 `// SAFETY:` 注释
- W2 由主智能体执行注册 + 记事本手测清单（`20-assembly.md` §4）

## 5. 槽位

- `CandidateUi` trait 即 M4 WebView 候选窗槽位；TSF 层只持有 `Box<dyn CandidateUi>`
- 引擎访问走 `Arc<Engine>`；M4 换 PipeBackend 时只改 `session_bridge` 的会话来源
- 按键映射表集中一个函数，M3+ 加双拼/快捷键只动这里

## 6. 子智能体启动提示词

```
你负责实现 Input 输入法 MVP 的 ime-tsf 管线模块（纯 Rust COM/TSF 文本服务 + 注册脚本，不含候选窗绘制）。
先读 D:\Projects\input\docs\plan\00-overview.md、01-contract.md、30-conventions.md，
再读任务书 D:\Projects\input\docs\plan\13-mod-ime-tsf-core.md 并严格执行。
只能创建/修改属主矩阵中 Agent D 的文件；ui/mod.rs 已冻结，候选窗一律走 CandidateUi trait（先用 NullCandidateUi 桩联调）。
TSF 语义可参考 github.com/RagibHasin/uo-keyboard（MPL-2.0，借鉴结构勿逐行抄）与微软 SampleIME。
接口/常量以 01-contract.md §5/§5.1/§7 为唯一权威。
完成后必须满足 DoD：cargo check -p ime-tsf 无 warning、cargo build -p ime-tsf --release 产出 DLL。
真机注册手测由主智能体在 W2 执行。
最终回复：改动文件清单 + 构建输出摘要 + 已知风险点（TSF 时序、unsafe 块清单）。
```
