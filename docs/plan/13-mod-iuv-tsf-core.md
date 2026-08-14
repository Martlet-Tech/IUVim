# 13 · 任务书 D：iuv-tsf 管线（COM/TSF + 注册）

> 属主文件：`crates/iuv-tsf/src/{lib,registration,log,session_bridge,composition}.rs`、`src/com/**`、`build.rs`、`scripts/{register,unregister}.ps1`、Cargo.toml 的 winres 配置
> 前置阅读：`00-overview.md`、`01-contract.md`（§5 接缝、§5.1 注册常量、§7 集成约定）、`30-conventions.md`
> **禁止**修改 `src/ui/mod.rs`（W0 冻结）。候选窗只通过 `CandidateUi` trait 使用，构造 `GdiCandidateWindow` 时调用 Agent E 提供的 `ui::gdi::GdiCandidateWindow::new() -> Self`（若 W1 期间 E 尚未完成，先用 `ui/mod.rs` 临时桩 `NullCandidateUi`——W0 已在 ui/mod.rs 提供该桩）。

## 1. 目标

产出可注册的 `iuv_tsf.dll`：纯 Rust 实现 TSF 文本服务，把按键流接到 `iuv_core::Session`，
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
- regsvr32 兼容：脚本直接 `regsvr32 /s iuv_tsf.dll`

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

引擎单例：`static ENGINE: OnceLock<Arc<Engine>>`，路径 `%LOCALAPPDATA%\iuv\iuv.imedic`
（`SHGetKnownFolderPath(FOLDERID_LocalAppData)` 或环境变量 `LOCALAPPDATA` 拼接）。加载失败记日志，
进入"透明模式"：全部按键放行。

**中英切换 = 系统机制**（2026-08-12 落地，d44487a）：`GUID_COMPARTMENT_KEYBOARD_OPENCLOSE`
compartment 为真相源——系统「输入法/非输入法切换」热键（Ctrl+Space，用户可自设）驱动
`ITfCompartmentEventSink::OnChange`，语言栏点击归一为写该 compartment；**Shift 临时英文方案
已移除**（依赖 app 路由键进 TSF，notepad/钉钉失效）。前置条件：用户把"输入法/非输入法切换"
设为 Ctrl+Space（"切换输入语言"热键让位，Win+Space 仍可用）。

### 3.4 `session_bridge.rs`：按键映射 + Effect 应用（契约 §7）

- vk → `Key`：`VK_A..VK_Z`（无 Shift）→`Char(小写)`，带 Shift/CapsLock →`ShiftChar(大写)`（XOR 判定，大写保形进序列——`niHAO` 候选仍从 `ni` 出，commit 原样上屏）；`VK_OEM_7`(`'`)→`Char('\'')`；`VK_BACK/SPACE/RETURN/ESCAPE/PRIOR/NEXT/UP/DOWN/LEFT/RIGHT`→对应；`VK_1..VK_9`（无 Shift）→`Digit(n)`；`VK_LEFT/RIGHT+Shift`→`SwapLeft/SwapRight`（M2 主动调权）；`VK_DELETE+Shift`→`HideCandidate`（M2 隐藏）
- **修饰键约定**：Ctrl/Alt 按下时 `map_key` 一律返回 None（组合键如 Ctrl+S/Alt+F4 放行给应用，绝不消费；Alt 组合 = `WM_SYSKEYDOWN` 本就不进 TSF 键 sink）；仅 Shift 修饰参与映射（大小写/符号/方向键调权）
- `OnTestKeyDown` 规则：Session active → 上表内键一律吃掉；非 active → 仅字母键吃掉（开启会话，`is_session_start_key` 含 ShiftChar），其余放行
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

`log_line(msg)`：追加写 `%TEMP%\input-iuv-tsf.log`，带时间戳与进程名；全 crate 错误路径必记。
关键事件记日志：Activate、引擎加载结果、每次 commit、注册结果。

### 3.7 `build.rs` + 版本资源

`winres` 写入文件描述/版本/图标（图标可用默认空 ico，`res/` 下自带一个 1x1 占位）。

### 3.8 脚本

- `scripts/install.ps1`：复制 `target\release\iuv_tsf.dll` 到 `%ProgramFiles%\iuv\`（自建目录）、
  `data\iuv.imedic` 到 `%LOCALAPPDATA%\iuv\` → 未注册则 `regsvr32 /s` → 经受限计划任务重启 ctfmon →
  提示切输入法验证；DLL 被占用时 MoveFileEx 排队替换（重启生效），不杀进程不关应用
- `scripts/uninstall.ps1`：删注册表键 + MoveFileEx 排队清理占用 DLL + 受限重启 ctfmon
- 两脚本检测管理员权限，非管理员给出明确报错

## 4. 测试与 DoD

TSF 难以自动测，DoD 为构建级 + W2 手测：

```
cargo check -p iuv-tsf                 # 无 warning
cargo build -p iuv-tsf --release       # 产出 iuv_tsf.dll
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
你负责实现 iuv 输入法 MVP 的 iuv-tsf 管线模块（纯 Rust COM/TSF 文本服务 + 注册脚本，不含候选窗绘制）。
先读 D:\Projects\vaim\docs\plan\00-overview.md、01-contract.md、30-conventions.md，
再读任务书 D:\Projects\vaim\docs\plan\13-mod-iuv-tsf-core.md 并严格执行。
只能创建/修改属主矩阵中 Agent D 的文件；ui/mod.rs 已冻结，候选窗一律走 CandidateUi trait（先用 NullCandidateUi 桩联调）。
TSF 语义可参考 github.com/RagibHasin/uo-keyboard（MPL-2.0，借鉴结构勿逐行抄）与微软 SampleIME。
接口/常量以 01-contract.md §5/§5.1/§7 为唯一权威。
完成后必须满足 DoD：cargo check -p iuv-tsf 无 warning、cargo build -p iuv-tsf --release 产出 DLL。
真机注册手测由主智能体在 W2 执行。
最终回复：改动文件清单 + 构建输出摘要 + 已知风险点（TSF 时序、unsafe 块清单）。
```
