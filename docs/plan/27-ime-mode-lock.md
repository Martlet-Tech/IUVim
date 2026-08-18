# 27 中英模式锁定（黑名单强接管，Plan B2）

> 2026-08-19 决策记录。问题诊断见下，决策：**B2（黑名单 app 内强接管、跟随用户切换）**。
> Plan A（daemon 全局锁，XP 时代体验）为另一维度，**暂缓**，方案附录。

## 1. 问题诊断（Altium 实测）

- `GUID_COMPARTMENT_KEYBOARD_OPENCLOSE` 是 **线程管理器作用域** compartment，各 app 各持一份
  → 跨 app 中英状态天然独立（浏览器英文、记事本中文）。
- 更细一层：TSF 在**输入上下文切换**时恢复每个上下文自己存的 open 状态。Altium 的
  Delphi/TServer UI 无数内部控件，焦点移动触发大量上下文切换 → **app 内中英胡乱变**
  （日志 `iuv-tsf.log` 6021.237→6021.240 的 3ms 双翻：外部写者两次写入）。
- 外部佐证：Altium 代理商亿道 FAQ（emdoor.cn/Res/view/id/511.html）明言
  "搜狗和微软中文输入法在 AD 不行"——所有中文输入法通病，非本输入法 bug。
- 用户抱怨点分两个维度：
  1. 跨 app 各自独立（浏览器/记事本）→ 治本需全局锁（Plan A）。
  2. 单个 app 内胡乱变（Altium）→ 治本需抑制该 app 的篡改（Plan B）。

## 2. 决策

- **选 B2**：黑名单 app 内强接管 compartment，跟随用户切换（用户可 Ctrl+Space 切换，
  切换后锁定不被 app 篡改）。
- **Plan A 暂缓**：daemon 全局锁（所有 app 从 daemon 拉取用户期望值）单独立项，
  见文末附录。
- **A/B 关系**：两个维度，可独立实现。B 不依赖 daemon，纯 TSF 侧改动。

## 3. B2 设计（黑名单强接管）

```jsonc
// config.json 新增（同 passthrough_apps 匹配方式：exe 名，大小写不敏感精确匹配）
"ime_lock_apps": ["X2.EXE", "..."]
```

黑名单进程内（该 TSF 实例）独占 OPENCLOSE compartment：

1. **注册 Ctrl+Space 为 preserved key**（`ITfKeystrokeMgr::PreserveKey`，自定义 GUID）
   → 用户切换走 `OnPreservedKey`，由 TIP 翻转 pinned 并写 compartment。
   **用户切换识别是确定性的，零启发式**——这是 B2 相对 Plan A 的最大优势。
2. **OnChange 一律重锁**：compartment 变到 != pinned → 立即 `write_openclose(pinned)` 写回。
   因用户切换已被 preserved key 接管，任何外部写入（Altium 的）必然"非用户"→ 直接压制。
3. **正确性关键**：外部写入触发的 OnChange **不走** `apply_openclose(local)`（否则误翻转
   english_mode、甚至 flush 进行中的拼音会话）——只重锁，保持 `english_mode = !pinned` 不变。
4. pinned 初始 = 中文（open=true，同现有"激活即打开"）；用户 Ctrl+Space / 点语言栏才更新。
5. 与 `passthrough_apps` 冲突时 passthrough 优先（黑名单不生效）。

**非黑名单 app 行为不变**（维持 Windows 每 app 原生语义），不注册 preserved key、零副作用。

## 4. Phase 1 可行性诊断（先做）

**两个命门，都需在 Altium 实测：**

| # | 问题 | 验证 | 失败后果 |
|---|---|---|---|
| Q1 | 系统"输入法/非输入法切换"是否把 Ctrl+Space 让给 TIP 的 preserved key（`OnPreservedKey` 能收到）？ | 注册 + 日志 | 收不到 → B2 干净版不成立，退回"Ctrl 键态 + 焦点窗口"启发式（复杂度回到 A 水平） |
| Q2 | 上下文切换窗口内写回 pinned 是否停住（不被 TSF 再断言回去）？ | 重锁后 +0/+20/+100ms 读回日志 | 写回被吞 → 加延时重锁兜底（PostMessage 隐藏窗） |

**诊断代码改动：**
- `crates/iuv-core/src/config/mod.rs`：`ime_lock_apps: Vec<String>` + 解析测试（顺手做掉）。
- `platforms/windows/iuv-tsf/src/com/text_service.rs`：
  - Activate 检测黑名单（复用 `is_passthrough_app` 匹配逻辑）→ `PreserveKey` 注册 + 日志；
  - `OnPreservedKey` 日志（rguid / 时间戳）；
  - `OnChange` 日志（local/pinned/Ctrl 键态/距上次 OnSetFocus/Push/Pop 毫秒）；
  - ime_lock 时执行重锁 + 读回日志。

**手测脚本（Altium）：**
1. 按 Ctrl+Space → 日志有无 `OnPreservedKey`。
2. 中文态下点不同控件/面板 → 日志重锁是否触发、托盘是否稳在中文。
3. 数值输入框打字确认无卡死。

**决策门**：Q1/Q2 全过 → B2 完整实现；Q1 不过 → 评估启发式方案或放弃。

## 5. 实现步骤（诊断通过后）

1. （已含在诊断）config 字段 + 测试。
2. Activate 黑名单检测 + PreserveKey 注册（非黑名单不注册）。
3. `OnPreservedKey`：Ctrl+Space → 翻转 pinned + 写 compartment + `apply_openclose`（含 flush，用户切换正常语义）。
4. `OnChange`：ime_lock 重锁（同步写回；Q2 不过则加延时兜底）。
5. 语言栏点击路径：ime_lock 下同步更新 pinned。
6. 单测：重锁/分类逻辑纯函数化；config 解析。
7. 手测：Altium 三连 + 普通 app 无副作用 + passthrough 优先级。

## 6. 风险

- preserved key 与系统热键冲突（Q1，待测）。
- 重锁写入在上下文切换窗口被吞（Q2，待测；延时兜底已备）。
- 黑名单 app 内英文输入受限（数值/热键字段被强制中文）——B2 既定代价。
- 与 Altium 互写若成高频竞争：单次写非循环（Altium 只在焦点事件写一次），手测确认无卡顿。

---

## 附录：Plan A（全局锁，XP 时代体验）——暂缓

> 目标：daemon 持有"用户期望中英模式"，任何 app 焦点切入（Activate / OnSetFocus）拉取并强制应用。
> 数据流：用户 Ctrl+Space → 焦点 app OnChange 分类 → `Request::SetMode` 写 daemon →
> daemon 更新共享段 `mode_open` + 持久化 config.json；各 app 拉取收敛。
> 与 B 的区别：B 治"单个 app 乱翻我"，A 治"跨 app 各持状态"。A 需 daemon/共享段/分类启发式，工作量更大。
> 待用户拍板后再立项。
