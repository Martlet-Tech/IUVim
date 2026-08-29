# OVERVIEW · iuvim 桌宠挂件 + Steam 创意工坊（Phase 1 交付）

> 2026-08-29 · MVP 开发专家团 Phase 1 调研交付

## 本次完成

- 三路并行调研：PM 竞品分析 / 架构师技术验证 / 设计师设计方向
- 三方一致性检查通过（功能 ↔ 架构 ↔ 设计 Token 全咬合）
- 三文档产出（docs/pet/ 下）：
  - `PRD.md` —— 市场空白、竞品矩阵、MVP 范围（3 个 P0）、商业模式（免费+创意工坊）
  - `ARCHITECTURE.md` —— 选型矩阵（tiny-skia 演进 / mlua-Luau 沙箱 / steamworks-rs / 同窗渲染）、架构分层、风险处置
  - `UIUX.md` —— 双寄存器设计语言、深色 Token、栖木式融合、Lucide 图标锁定

## 关键决策（用户已拍板）

1. 工具栏 = 本体，宠物 = 可互动挂件
2. 免费上架 Steam + 创意工坊（Lua + 2D 精灵帧扩展）
3. 离线时已下载 mod 继续可用（Wallpaper Engine 模式）
4. 实现优先，不做进程隔离（线程内 Lua + 兜底三件套）

## 下一步（等用户确认三文档）

确认后自动推进：Spec 生成 → 设计细化 → 并行开发 → 测试交付
