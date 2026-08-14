# platforms/macos — macOS 平台层（占位）

> **状态：占位**。尚未开发。引擎线（iuv-core/iuv-data/iuv-repl）已跨平台，可直接复用。

## 待开发内容（真做时再建 crate）

### 1. 系统适配层：InputMethodKit（IMK）
- macOS 输入法 = `IMKInputController` 子类（Objective-C/Swift 侧）+ Rust 引擎
- 按键流 → iuv-core `Session::on_key` → `Effect` → IMK `setMarkedText`/`insertText`
- 已知工程点：macOS 应用进程内加载 Rust 静态库（`cdylib`/`staticlib`）、
  IMK 线程模型（主线程回调）、sandbox 与权限（`com.apple.inputmethod` 协议）
- 替代调研：`InputMethodKit` 官方 API 自 10.6 稳定，无 Windows TSF 的机制地雷

### 2. 门面（候选窗）
- IMK 自带候选条（`IMKCandidates`）或自绘 `NSWindow` 两个方向，开发时择一
- 复用 `UiSnapshot` 数据契约（跨平台中立），渲染端各平台自管

## 与 Windows 的对应关系

| 层 | Windows | macOS |
|---|---|---|
| 系统适配 | `iuv-tsf`（TSF） | IMK 插件（未建） |
| 门面 | GDI（现）/ helper D2D（M4） | IMK 候选条 / NSWindow（未建） |
| 引擎 | iuv-core（共用） | iuv-core（共用，零改动） |
