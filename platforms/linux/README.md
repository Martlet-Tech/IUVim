# platforms/linux — Linux 平台层（占位）

> **状态：占位**。尚未开发。引擎线（iuv-core/iuv-data/iuv-repl）已跨平台，可直接复用。

## 待开发内容（真做时再建 crate）

### 1. 系统适配层：Fcitx5（首选）/ IBus（备选）
- Linux 输入法生态碎片化：Fcitx5 与 IBus 两套，多数桌面默认 IBus（GNOME）、
  KDE 与中文用户常用 Fcitx5
- Fcitx5：Rust 插件有先例（fcitx5 提供 C++ ABI，可用 `ffi` 对接；社区有 rust crate 探索）
- IBus：DBus 接口 + `IBusEngine` 协议
- 决策点（开发时）：先 Fcitx5 单平台，还是双后端抽象

### 2. 门面（候选窗）
- Fcitx5 自带候选窗（插件只提供候选数据）→ 门面成本最低
- IBus 候选窗由面板实现（GNOME Shell 扩展）
- 自绘门面（如需自定义皮肤）：**M4 起复用 `crates/iuv-ui` 渲染栈**（tiny-skia + cosmic-text + Theme），
  只写 X11/Wayland 窗口层

## 与 Windows 的对应关系

| 层 | Windows | Linux |
|---|---|---|
| 系统适配 | `iuv-tsf`（TSF） | Fcitx5 / IBus 插件（未建） |
| 门面 | iuv-ui 绘图 + D2D/DComp 呈现（M4） | Fcitx5 自带候选窗 / iuv-ui 自绘（未建） |
| 引擎 | iuv-core（共用） | iuv-core（共用，零改动） |
