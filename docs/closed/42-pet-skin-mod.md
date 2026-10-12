# 42 号任务书 · M2a 桌宠素材 mod 化

> 状态：计划定稿，**待实施**（本次仅落档，管理员确认后动工）。
> 范围：仅素材 mod（皮肤目录发现/选择/热切换/兜底 + 设置页）；Lua 行为脚本（mlua）为 M2b，下一轮。
> 定位：本目录格式即未来 Steam 创意工坊（M3）的 mod 内容格式基础——现在铺轨，上架时直接用。
> 已拍板：①仅 M2a ②内置 girl_default 永久兜底 ③设置页新增「桌宠」标签页 ④改完不提交。

## 1. 用户体验目标

把皮肤文件夹（`skin.json` + 分层 PNG）放进 `%LOCALAPPDATA%\iuv\pet\skins\<名字>\`
→ 设置页「桌宠」标签出现该皮肤 → 勾选应用 → 桌宠即时切换；
目录删除/损坏 → 秒回内置 girl_default + 日志记录。

## 2. 现状（已勘察确认，勿重做）

- `pet_assets.rs` 已有完整外部皮肤加载：`external_skin_dir(id)`（`<iuv_dir>/pet/skins/<id>/`）、
  `load_skin_dir(dir)`（skin.json + 分层 PNG + 缺层降级）、三级装配（外部→内置→L0 像素狗）。
  **缺口仅四**：多皮肤枚举（现硬编码 DEFAULT_SKIN_ID）、按选择装配、热切换、设置页 UI。
- `DaemonConfig` 加字段模式成熟（hide_on_fullscreen 先例）；**save_config 的 json! 回退分支必须同步加字段，易漏**。
- `BarEvent` FIFO 串行消费；`PetArt` 已跨线程移交（Send+Sync 成立），携带 `Arc<PetArt>` 变体零障碍。
- `ToolbarWindow` 持有 pet_art / pet_anim / pet_model / pet_mask(+w/h) / pet_rect / pet_down——
  热切换需**成组失效**（mask、rect、down 三个派生缓存全清）。
- **关键坑**：`repaint()` 传旧窗口矩形，不重设窗口尺寸；`UlwSurface::upload` 的 psize 才会重设。
  换肤若 design_size 变化，repaint 会把新画面**拉伸进旧窗口** → 热切换必须走 `hide()+show()`。
- 设置页 `tabs()` 的 `BASE: [Tab; 5]` 定长数组 → 加 Tab::Pet 后 5→6，编译器强制同步。
- 高级页缺外层 ScrollArea 的溢出 bug 已挂账（本任务书不动）；**新标签必须自带 ScrollArea**
  （范本 = keymap_tab：`max_height(ui.available_height() - 12.0)` + id_salt）。

## 3. 设计要点

1. 枚举/校验为纯函数 `scan_skins_in(base) -> Vec<SkinEntry{id,dir,valid,reason}>`（可单测）。
2. 装配双入口：`try_load_pet_art(id) -> Option<PetArt>`（设置页用，失败提示）+
   `load_pet_art(id) -> PetArt`（启动用，失败静默回落内置 + 日志）。
3. `"girl_default"` 语义保持现状：外部同名目录可用则遮蔽内置；列表首项显示「内置默认」。
4. 热切换走 FIFO：`BarEvent::PetSkinChanged { art: Arc<PetArt> }`，与焦点/打字事件同队列串行，无撕裂。
5. config 只存字符串 + 字符级清洗（拒绝路径分隔符），可装载性在装配时判定回落。
6. 设置页原子性：装载失败 → **整体中止本次 apply**（其他字段也不保存），避免三方不一致。
7. `is_valid_skin_id`：字母数字下划线连字符、1..=64、拒绝路径分隔符——枚举过滤与 config 清洗共用。

## 4. 实施步骤（T01→T08 依赖序）

| 步骤 | 文件 | 内容 |
|---|---|---|
| T01 | `pet_assets.rs` | is_valid_skin_id + scan_skins_in 枚举 + 单测（临时目录造假皮肤） |
| T02 | `pet_assets.rs` | load_pet_art 泛化带参 + try_load 双入口 + 现有测试同步 |
| T03 | `config.rs` | `pet_skin` 字段（默认 "girl_default"）+ 读写双分支 + 往返测试 |
| T04 | `toolbar/mod.rs` `window.rs` | PetSkinChanged 事件 + 成组替换（PetAnim 重建、三缓存清空、**hide+show**） |
| T05 | `main.rs` | 启动装配改带参（config 先读再装配） |
| T06 | `settings.rs` | Tab::Pet + tabs() 5→6 + 桌宠标签（自带 ScrollArea：单选列表、无效灰显带原因、打开皮肤目录、刷新）+ apply 原子切换 |
| T07 | 文档 | status.md 台账 + ARCHITECTURE §3 mod 管理器行改"部分实现（素材 mod）" |
| T08 | 验证 | cargo check/test 全绿；**不提交**，管理员手测后另行提交 |

## 5. 风险

| # | 风险 | 对策 |
|---|---|---|
| 1 | repaint 不重设窗口尺寸 → 换肤拉伸 | T04 强制 hide+show（唯一以 surf 尺寸上屏的路径） |
| 2 | 设置页主线程装载卡顿 | 一次性点击、十毫秒级可接受，不做后台线程 |
| 3 | 恶意目录名/路径注入 | is_valid_skin_id 白名单 + config 清洗 |
| 4 | 切换瞬间动画状态错乱 | 重建 PetAnim + 重置 PetModel（同 FocusGained 切实例既有语义） |

## 6. 验收

- [ ] 设置页「桌宠」列出内置 + 用户皮肤，无效皮肤灰显带原因
- [ ] 自制皮肤（含尺寸不同者）应用后即时切换、不拉伸
- [ ] 删除启用中皮肤目录后重启 → 秒回内置，日志有回落记录
- [ ] workspace 全绿；未提交任何变更
