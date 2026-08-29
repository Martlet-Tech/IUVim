# 架构文档 v1.0 · iuvim 桌宠挂件 + Steam 创意工坊

> 生成日期：2026-08-29
> 状态：待用户确认（Phase 1 门禁）
> 基于：PRD v1.0 + 架构师 Phase 1 技术调研
> 结论先行：**整体可行，无不可行项**。技术栈在现有 iuvim（Rust + tiny-skia + TSF）上演进，零重型新依赖。

---

## 1. 技术选型矩阵（已定）

### 1.1 桌宠动画渲染 → tiny-skia 演进（同栈）

| 方案 | 结论 |
|------|------|
| A. tiny-skia 演进（**选定**） | 桌宠 = 精灵帧贴图序列，现有 `render_to_surface`/`draw_pixmap` 缩放+混合管道已够；<256px 精灵 30fps CPU 完全胜任；零新依赖、与 Theme 皮肤体系天然融合、常驻低功耗（空闲停帧 + 脏区重绘，消息泵现成）。新增 `pet.rs`：sprite sheet 切割 → Pixmap 序列缓存 → 帧率控制 |
| B. wgpu | 2D 精灵用 GPU 过度设计；依赖树大（naga/gpu-alloc）；ULW 是 GDI 位图窗口与 wgpu 交换链不兼容需另做桥接；常驻 GPU 上下文增功耗与崩溃面。否决 |
| C. softbuffer | 官方明确只提供像素 buffer 无绘制原语，需配 tiny-skia；对 ULW 场景零增益。否决 |

### 1.2 Lua 嵌入 → mlua（Luau 特性，vendored）

| 方案 | 结论 |
|------|------|
| A. mlua（**选定**） | 活跃（v0.12）；支持 Luau 特性，官方沙箱专为不可信代码设计（Roblox 同款）：移除 io/package/debug/dofile/loadfile、全局只读、`set_interrupt` 防死循环、字节码可加密。创意工坊 mod 安全首选 |
| B. rlua | 已废弃，0.20 仅 mlua 薄包装。排除 |
| C. rhai | 纯 Rust 集成最易但玩家要学新语言、生态小、沙箱是设计取向非严格隔离。备选 |

**热加载**：mod 变更 → 重新编译执行。API 绑定成本中等（UserData/create_function 标准流程）。

### 1.3 Steamworks 集成 → steamworks-rs v0.12.0（SDK 1.62）

- UGC/Workshop API 齐全：create_item/start_item_update/submit、subscribe_item/subscribed_items、item_download_info/install_info、query_all 等。
- **上架核查结论**：
  - 费用 = $100 Steam Direct 一次性（免费软件同样收，累计收入 $1000 后返还）
  - 应用类型可注册 Software（软件）免费档，无收入即无分成
  - **仅 Windows 完全允许**（官方 FAQ 明确，商店页只勾 Windows）
  - Workshop 按 AppID 工作，需 App Admin → Workshop 配置 → Enable ISteamUGC + Steam Cloud 配额
  - 需 W-8BEN 税务 + 银行验证 + 年龄分级（免费软件走 IARC 免费档）
- 关键约束：`Client::init()` 需 Steam 客户端运行且经 Steam 启动 → 离线降级设计（见 §6）。

### 1.4 宠物窗口架构 → 同窗渲染

| 方案 | 结论 |
|------|------|
| A. 同窗渲染（**选定**） | 宠物并入工具栏 ULW 窗口，居中挂工具栏正上方（2026-08-30 起不再向右追加宠物区），一个窗口一个消息泵；吸附天然成立（同一表面）；拖拽 = 现有整窗拖拽复用；点击穿透走 ULW alpha；四态联动 = daemon 现有实例表/focused，状态变化 → 宠物动画状态机 → 帧渲染。零新窗口零新泵 |
| B. 独立子窗口 | 需第二消息泵 + 双窗位置同步 + 双份穿透/置顶协调，仅「宠物可脱离工具栏独立放置」需求出现时才做。MVP 否决 |

### 1.5 跨平台 → 明确结论

Steam 允许仅 Windows，无需 macOS/Linux。iuv-core/iuv-ui 已分层，未来加平台适配层即可，非 MVP 范围。

## 2. 架构分层（现有 iuvim 演进）

```
┌───────────────────────────── Steam 客户端（运行时可缺失） ─────────────────────┐
│  steamworks-rs：创意工坊 UGC（订阅/下载/上传/浏览）                              │
└──────────────┬───────────────────────────────────────────────┬────────────────┘
               │ Steam 在线：mod 同步       │ Steam 离线：本地 mod 目录直载
               ▼                                               ▼
┌───────────────────────────── iuv-daemon（唯一持有） ───────────────────────────┐
│  复合工具栏窗口（ULW）：工具栏区 + 宠物区（同窗） 动画定时器（仅动画激活时 tick）   │
│  实例表 {pid:tid → {state, active}}  focused  mod 管理器（本地持久化）           │
│  pet_runtime：mlua(Luau) 沙箱 + 脚本 API + 指令上限 + panic 捕获                │
└──────┬───────────────────────────────────────────────┬────────────────────────┘
       │ TSF→daemon：Register/StateSync/Active（现有单向管道）
       │ daemon→TSF：Cmd::SetState（现有反向通道）
       ▼
┌───────────────────────────── TSF 每实例（per 窗口/线程） ──────────────────────┐
│  TextService.runtime: Arc<RuntimeState{mode,width,script,punct}>（现有）         │
└────────────────────────────────────────────────────────────────────────────────┘
```

## 3. 模块清单（改动面）

| 模块 | 位置 | 内容 |
|------|------|------|
| 宠物渲染 | `crates/iuv-ui/src/pet.rs`（与 toolbar.rs 同级） | sprite sheet 切割 → Pixmap 序列缓存 → 帧率控制 → 状态驱动动画（闲置/打字/中英/简繁换装等） |
| 宠物动画模型 | `crates/iuv-core/src/pet_model.rs` | 动画状态机纯逻辑（无 I/O，可单元测试） |
| Lua 运行时 | 新 crate `iuv-pet-runtime` | mlua(Luau) 沙箱封装 + mod 脚本 API；MVP 先做「素材 mod」（Lua 描述动画/行为参数），脚本沙箱二期再开 |
| mod 管理器 | `iuv-daemon` | 本地 mod 目录扫描/加载/校验、Steam 订阅同步（在线时）、版本控制 |
| 复合窗口 | `iuv-daemon` 工具栏窗口扩展 | 工具栏区 + 宠物区同窗渲染、吸附（栖木式）、拖拽复用、点击穿透 |
| Steamworks | `iuv-daemon` 新增依赖 | steamworks-rs：UGC 订阅/下载/浏览/上传（P1） |
| 图标 | 源图 PNG 迁移 | Lucide path data 直接嵌入 Rust（tiny-skia Path 绘制），编译期 include_bytes 不变；桌宠动画素材仍为精灵帧 PNG |

## 4. 数据流：四态联动（核心）

```
TSF StateSync（mode/width/script/punct 变化）
    → daemon 实例表更新
    → PetModel 状态机迁移（如 mode=EN → 打盹偷瞄；打字 → 敲键盘动画）
    → pet.rs 帧渲染（脏区重绘）
    → 工具栏按钮图标同步（现有逻辑，宠物区与按钮区互不遮挡）
```

## 5. 图标方案（锁定）

- **图标库 = Lucide**（ISC 许可，可商用含 Steam 场景）：24×24 统一 2px 描边 + currentColor，与扁平细边框 UI 同构。
- 实现：Lucide 图标为 SVG path，提取 path data 直接用 tiny-skia `Path` 绘制 → **无需引入 resvg 依赖**（比调研阶段更轻的落地方式，保持零新依赖）。
- 规格：16px（行内）/ 20px（按钮内）/ 24px（独立图标），全项目统一。
- 桌宠动画素材 = 精灵帧 PNG（动画非图标，不受图标规范约束）。
- 全项目禁 emoji 作功能图标（P0）。

## 6. 风险处置（用户已拍板）

1. **Steam 离线降级**：mod 下载后本地持久化（`%LOCALAPPDATA%\iuv\mods\`），Steam 离线时已下载 mod 继续加载运行；Steam 在线时才做订阅同步/更新/新订阅拉取。默认宠（内置资源）永远可用。此为 Wallpaper Engine 模式。
2. **daemon 稳定性（实现优先）**：mod 脚本在独立线程跑 + `set_interrupt` 指令上限 + panic 捕获 → 兜底三件套；**不做独立进程隔离**（MVP 不做，生态起来后再评估）。
3. **UGC 版权**：UGC 条款 + 举报机制 + 工坊一律免费 Mod。

## 7. 性能目标

- 宠物动画：<256px 精灵、30fps、空闲停帧（零 tick）；常驻 CPU 增量 < 1%（空闲时）。
- 打字热路径零影响：宠物动画只在状态变化/动画激活时 tick，不触碰候选窗渲染管道。
- 全屏游戏检测：沿用候选窗隐藏策略，宠物动画同步暂停（Wallpaper Engine 模式）。

## 8. 里程碑（MVP 拆分）

| 里程碑 | 内容 | 出口标准 |
|--------|------|----------|
| M1 桌宠骨架 | pet.rs + pet_model + 复合窗口 + 默认宠动画 | 宠物挂工具栏、随四态反应、可拖拽点击 |
| M2 mod 运行时 | iuv-pet-runtime + mod 管理器 + 素材 mod 格式 | 本地加载/卸载 mod、沙箱生效 |
| M3 Steam 上架 | steamworks-rs + 工坊订阅/浏览 + 上传（P1）+ 商店页 | 免费应用上架，工坊可订阅下载 |

## 9. 变更记录

| 日期 | 变更内容 | 原因 | 影响范围 |
|------|----------|------|----------|
| 2026-08-29 | v1.0 初版 | Phase 1 调研结论汇编 | — |
