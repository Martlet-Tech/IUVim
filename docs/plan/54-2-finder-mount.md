# 54-2 · 桌宠挂载实施（Finder 捕捉 + 挂靠跟随）

> 状态：**MVP 七步全部落地**（2026-10-14，分支 `feat/54-2-finder-mount`）。
> 前置：`54-pet-mount.md`（方向 / 机制 / 四宠实测档案）。本文是**施工方案**——
> 目标 = 可靠可跑的 MVP，**不含锦上添花**。§7 的决策已冻结，实施中不回头讨论。

## 1. MVP 范围

**做**

1. 捕捉：双路径（准星 + 枚举兜底）
2. 捕捉 UI：取景框 + 目标描边 + 提示条
3. 目标档案 + 持久化
4. 挂靠跟随：WinEventHook 四事件 + 分轴 clamp
5. 拖拽改 offset
6. 设置页「追踪宠物」标签页（当前目标信息 + 解锁）

**不做（明确出局）**：跟随策略分级 · 反向驱动宠物（`SetWindowPos(pet)`）·
取景框入场动画 · 方向键切换候选窗口 · 分离态视觉提示 · 弹性/阻尼跟随 ·
全屏遮罩（改用描边，无 DPI 债）· 管理员权限宠物的挂载（只标不可挂）。

## 2. 锚点与跟随模型（核心，冻结）

```
offset    : 唯一真相源（只有用户拖拽能写）
petAnchor : 宠物窗口并集矩形上的 9 宫格点（屏幕坐标）
barAnchor : 工具栏自身坐标系的 9 宫格点
desired   = petAnchor + offset − barAnchor
visual    = clamp(desired, 虚拟桌面矩形)      ← 分轴
```

- 宠物移动 → 只有 `petAnchor` 变 → `visual` 平移（未夹紧时严格等于 offset）
- 夹紧**从不回写 offset**；宠物回到「开始被夹住时的位置」自动恢复精确跟踪
  ——**无状态**，不记历史量
- 分轴夹紧 ⇒ 触底后仍沿该边滑行（宠物往右下走，y 被夹住、x 继续跟）
- 拖拽：暂停跟随 → 松手按**视觉位置**反推 `offset`，并**按方位重判锚点对**
  （余量进 offset）
- 工具栏侧锚点用**窗口矩形**，不求视觉边界——对齐由用户拖拽完成
- 多窗口目标（Shimeji 一宠两窗）取**并集矩形**；目标隐藏时暂停更新位置

## 3. 捕捉流程

**进入**：工具栏瞄准镜按钮（`TB_FINDER`，倒数第二、齿轮左边）

### 3.0 代码落点（与初稿的偏差）

| 位置 | 放什么 |
|---|---|
| `crates/iuv-ui/src/finder.rs` | 三个渲染纯函数（跨平台、无 Win32、可单测） |
| `iuv-win/src/window_probe.rs` | 枚举 / 过滤 / z 序命中 / 显示器 rect / 可挂性（Win32 只在这里） |
| `iuv-win/src/finder.rs` | 三窗 + 定时器轮询 + 会话状态机 |
| `iuv-win/examples/{probe,capture}.rs` | **免部署**真机验证入口 |

捕捉模块从初稿的 `iuv-server/daemon/finder/` **改放 `iuv-win`**：它全是 Win32 窗口/定时器
/ULW 管道（与 `popup.rs` 同类），且 iuv-win 是 lib —— 于是能挂 example 直接跑，
**Step 2/3 都不用部署输入法就能验收**。iuv-server 侧只留一行 `finder::start(theme, cb)`。

### 3.1 三件装饰件

| 件 | 形态 | 行为 |
|---|---|---|
| 取景框 | 光标所在**显示器**四角 L 形圆角括号（主题蓝），内缩 16px | 静态；换屏 `SetWindowPos` |
| 目标描边 | 目标 rect 外扩 2px，内部全透明只画边框 | 目标切换 / 尺寸变化才重绘 |
| 提示条 | 小 ULW，内容 `进程名 · 窗口类 · 尺寸 · 可挂性` | 跟光标 `SetWindowPos`，只移动不重绘 |

描边颜色分级：**可挂 = 主题强调色；不可挂 = 警示色**。

### 3.2 命中检测

`EnumWindows` + 过滤 + **自算 z 序取最上层**。

**不用 `WindowFromPoint`**——它跳过 `WS_EX_TRANSPARENT`（PixPin 实测抓不到
MateEngine，同一条 API 语义）。枚举兜底同时是同构多窗口唯一可选项。

**必须过滤自己**：排除本进程 PID（取景框 / 描边 / 提示条 / 工具栏 / 候选窗
都是自家的）——否则最上层永远是自家窗口，会「自己高亮自己」。

### 3.3 可挂性判定（建档案时即判定）

| 判定 | 条件 | 提示 |
|---|---|---|
| 可挂载 | — | 正常描边 |
| 全屏覆盖 | 盖满显示器 **且** `LAYERED\|TRANSPARENT` | 全屏覆盖 · 不可挂载 |
| 权限不足 | **进程路径拿不到**（受保护进程） | 不可挂载（权限不足） |

> 权限判据用「路径拿不到」而**不是** `OpenProcess(PROCESS_QUERY_INFORMATION)` 试开：
> 后者依赖**调用方自身完整性级别**（Step 2 真机实测：同一探针进程对 javaw 可开、
> 对 bongocat-app 不可开 → 误报）。且提权本身**不阻塞 MVP 路径**——读位置
> `GetWindowRect` 与 `SetWinEventHook` 都不过 UIPI（UIPI 拦的是跨完整性窗口消息，
> 而本方案不做 `SetWindowPos`/`SendMessage` 驱动宠物）。真正卡住的是**建档案**。
> （54 号 §4.3「管理员权限桌宠触发 UIPI」在 MVP 范围内不成立，见 §9 实测记录。）

### 3.4 输入

~16ms 定时器轮询 `GetCursorPos` + `GetAsyncKeyState`：左键 = 确认，
右键 / `Esc` = 取消。

**不抢焦点、不用 `SetCapture`、不用 hook**；`Esc` 只在捕捉态内处理，
不牵动其他代码。

## 4. 目标档案

```
{ 进程路径, 类名, 尺寸提示, exStyle 掩码 }
```

**匹配规则**（真机实测逼出来的，见 §9）：

- 主键 = **进程路径 + 类名**（路径比较大小写不敏感）
- **尺寸只作消歧提示，不作硬条件**——BongoCat 实测窗口尺寸会变
  （350×203 → 280×162，1.25 倍缩放），拿尺寸当键会导致"用户一改缩放就找不到挂过的宠物"
- 消歧顺序：尺寸最接近档案者 → 并列取 **z 序最上层**
- 同构多窗口（Shimeji 一群 mascot）**无法稳定锁定某一只**——MVP 已知边界
  （用户侧把 Shimeji 设成单只即可规避；多只语义留待后续）

**持久化落点**：`%LOCALAPPDATA%\iuv\mount.json`（与 `toolbar.json` 同目录；
"临时文件 + rename 原子替换"，与 toolbar prefs 同套路）。锚点 / offset 是 Step 6 追加
字段，届时配 `#[serde(default)]` 保证旧档案可读。

**API**（`iuv_win::mount`）：`from_probe` / `load` / `save` / `clear` / `find_target`。
解锁（主动解绑）→ `clear()`；被动解挂见 §5。

## 5. 生命周期

**事件**（`SetWinEventHook`，绑目标 PID）：`LOCATIONCHANGE` + `SHOW` + `HIDE` + `DESTROY`。
只挂 `LOCATIONCHANGE` 不够——宠物可见性是动态的（Shimeji 实测 8 窗 7 隐），
否则宠物藏起来时工具栏留在原地飘着。

**被动解挂**（目标关闭 / 崩溃 / 隐藏）→ 工具栏**留在原地当普通工具栏**（offset 与姿态保留）。

**主动解挂** → 设置页「追踪宠物」标签页的**解锁**按钮。

## 6. 落地顺序（每步可独立验收）

| 1 ✅ | `iuv-ui` 三个渲染纯函数 + 单测 | `cargo test -p iuv-ui` 全绿（48 过） |
| 2 ✅ | `iuv-win/window_probe.rs`（枚举 / 过滤 / z 序 / 显示器 rect / 可挂性） | 单测 + 真机探针（见 §9） |
| 3 ✅ | 捕捉态三件装饰件 + 轮询输入（**落 iuv-win**，见 §3.0） | 真机 example 跑通（见 §9） |
| 4 ✅ | 工具栏瞄准镜按钮（素材 `assets/toolbar-icons/finder-target.png`） | 需部署：点按钮进入捕捉、点后日志出 `[finder]` |
| 5 ✅ | 档案 + 持久化（`iuv-win/mount.rs`，`%LOCALAPPDATA%\iuv\mount.json`） | 需部署：捕捉后出 `[mount] 档案已保存` + `立即找回：命中`；重启后出 `启动档案` |
| 6 ✅ | 挂靠跟随（四事件 + 分轴 clamp + 拖拽改 offset） | 需部署：拖宠物工具栏跟随；触底不越屏；拖工具栏改姿态 |
| 7 ✅ | 设置页「追踪宠物」标签页 | 需部署：标签显示当前目标 + 解锁按钮生效 |

## 7. 冻结决策速查（防反复横杠）

| 主题 | 决策 |
|---|---|
| 方向 | 宠物是宿主、工具栏是挂件；**只跟随，不驱动宠物** |
| 锚点 | `工具栏锚点 = 宠物锚点 + offset`；宠物侧取**窗口几何** 9 宫格 |
| offset | 唯一真相源；唯一写入口 = 用户拖拽 |
| 夹紧 | 分轴 clamp 到虚拟桌面；**从不回写 offset**；无状态 |
| 跟随 | 一条规则：宠物动 → 工具栏动 |
| 捕捉 | 双路径；**枚举兜底为默认命中手段** |
| 捕捉 UI | 描边式（**无遮罩**）：取景框 + 描边 + 提示条 |
| 输入 | 轮询；不抢焦点；`Esc` 仅捕捉态内 |
| 被动解挂 | 工具栏**留在原地当普通工具栏** |
| 默认姿态 | 右上外侧 |
| UIPI | 列出但标「不可挂载（权限不足）」 |

## 8. 待定

无。（档案持久化落点已定：`%LOCALAPPDATA%\iuv\mount.json`。）

## 9. 实测记录

### 2026-10-13 · Step 2 探针（真机，**无需部署**）

探针 = `cargo run -p iuv-win --example probe -- [--watch | --list | --at X,Y]`，
直接在当前桌面上验证窗口探测，不必装输入法。

**已验证**

- 命中检测正确：`--at 2921,699` 精确命中 BongoCat 宠物窗
  （`bongocat-app.exe · BongoCatProductOverlayWindow · 350×203`）
- **两遍扫描规则有效**：主屏被 `TextInputHost.exe`（1600×900）、DingTalk
  （1521×900）等多个满屏窗口覆盖，`pick_hit` 仍稳定取最上层那个，没有被吞
- 自家窗口按 PID 排除（探针进程 ≠ iuv-server，故工具栏会出现在探针列表里——
  属预期；真实 finder 跑在 iuv-server 内，工具栏/候选窗/装饰件同进程一并排除）

**新发现（补 54 号档案）**

- **BongoCat 也是多窗口**：除宠物 overlay（350×203）外还有一个可见主窗
  `Zed::Window`（1076×632，GPUI 系命名）。即"一个宠物多个候选窗口"不是 Shimeji
  独有，BongoCat 同样需要归属判定。
- **权限判据修正**（见 §3.3 注）：`PROCESS_QUERY_INFORMATION` 试开受调用方完整性
  影响，实测对 `bongocat-app` 误报"权限不足"；改用"进程路径拿不到"。
  推论：54 号 §4.3「管理员权限桌宠触发 UIPI」在 MVP 范围内**不成立**。

**待补测**

- MateEngine 的"全屏覆盖"判定（探针运行时它已退出，需重启后复测）
- Lively 那档（壁纸层是否真无独立窗口）

### 2026-10-13 · Step 3 捕捉态（真机，**无需部署**）

入口 = `cargo run -p iuv-win --example capture`（左键确认 / 右键或 Esc 取消）。

**已验证**：进程起得来、三窗建成、定时器轮询持续运行不崩（连跑 6s 无异常）。
**未验证（需人眼）**：取景框/描边/提示条的观感、悬停跟随是否跟手、确认/取消手感。

实现要点（都为了不抢焦点）：

- 三窗统一 `TOPMOST|TOOLWINDOW|NOACTIVATE|LAYERED|`**`TRANSPARENT`**——点击穿透，
  不挡底下窗口的关闭按钮；鼠标交互**全走轮询**，不用 `SetCapture`、不用 hook
- 输入 = 16ms 轮询 `GetCursorPos` + `GetAsyncKeyState`；**首次轮询前等所有键松开**
  （否则"点工具栏按钮那一下"会被当成确认）——这是个必踩的坑
- 取景框仅换显示器时重绘；描边仅目标/尺寸变化时重绘；提示条**只 `SetWindowPos` 移动**
- 提示条位置夹到显示器矩形内（防溢出屏幕）

### 2026-10-13/14 · Step 4 工具栏按钮（**需部署验收**）

- 工具栏 6 → 7 按钮：`logo | 中英 | 全半角 | 标点 | 简繁 |`**`瞄准镜`**`| 齿轮`。
  `TB_FINDER = 5`、`TB_GEAR 5→6`、`TB_COUNT 6→7`（宽度 212 → 246px，公式自适应）
- 素材 `assets/toolbar-icons/finder-target.png`：288×288 RGBA8、主色 `#38A6F5`、
  墨迹 191×191 居中（与齿轮/字形同一规格）；造型 = 圆环 + 四向外伸刻度 + 中心点（瞄准镜）
- `on_click` 里瞄准镜**排在 focused 查询之前**——捕捉的是任意桌面窗口，不依赖 IME 实例
- Step 5（档案）接上之前，捕捉结果先落日志：
  `[finder] 选中：<进程> · <窗口类> · W×H @(x,y) · 可挂载性`
- 新增 `toolbar_icons.rs` 单测：全部内嵌素材必须能解码 + 源图同画布边长
  （守住「改图即生效」的工作流，换图出错立刻暴露而不是运行时按钮静默变空）

**已知未做**（不阻塞 MVP）：按钮在捕捉期间的"激活态"高亮——需要工具栏重绘联动，
当前靠取景框/描边/提示条承担模态提示（本就是主要信号）。

### 2026-10-14 · Step 1–4 真机验收通过 + Step 5

**Step 1–4 部署验收**（`dev-deploy.ps1` 后四次捕捉，全部成功、零失败零异常）：

```
[finder] 瞄准镜 → 进入捕捉态 / 进入捕捉态 / 退出捕捉态
[finder] 选中：C:\Program Files\Java\jdk-25.0.4.1\bin\javaw.exe · SunAwtWindow · 128×128 @(1829,812) · 可挂载
[finder] 选中：C:\Users\zt\AppData\Local\BongoCat\bongocat-app.exe · BongoCatProductOverlayWindow · 280×162 @(2817,531) · 可挂载
```

日志无解码失败 / `UpdateLayeredWindow` 失败 / panic；素材图标正常解码。

**实测暴露的两个修正**（都已落进 §4 / §9）：

1. **窗口尺寸不稳定** → BongoCat 从 350×203 变成 **280×162**（正好 1.25 倍缩放）。
   直接推翻"按 进程+类名+尺寸 匹配"的原计划 → 尺寸降级为提示，主键只留 进程+类名。
2. **昨日把 Shimeji 的 128×128 判成"菜单/弹窗"是错的**——它位置会变
   （(1457,768) → (1829,812)），**会移动的就是活体**，它是 Shimeji 本体。

### 2026-10-14 · Step 5 档案 + 持久化

`iuv-win/mount.rs`：`MountProfile{进程路径, 类名, 尺寸提示, exStyle}` +
`load/save/clear/find_target`，落 `%LOCALAPPDATA%\iuv\mount.json`（临时文件 + rename）。

- 捕捉确认后：存档案 + **立刻回读一轮**（`[mount] 档案已保存` / `立即找回：命中`），
  不重启就能证明"存得下、找得回"
- server 启动时：回读档案并试找回一次（`[mount] 启动档案` / `启动找回：命中|未命中`），
  证明持久化生效
- 7 条纯函数单测覆盖匹配语义：双主键缺一不可、路径大小写不敏感、
  **尺寸变了仍须找回**、尺寸最近者胜、并列取最上层、提示无效退化为取最上层、JSON 往返

### 2026-10-14 · Step 6 挂靠跟随

**6a（纯几何 + 档案姿态字段，零行为变化）**：`iuv-win/mount.rs` 新增
`anchor_point` / `follow_pos` / `clamp_pos` / `offset_from_visual` / `judge_anchor`。
档案追加 `anchor` / `offset`（`#[serde(default)]`，Step 5 的旧档案仍可读）。
三条关键单测：夹紧不回写 offset（回起点自动恢复）、分轴夹紧→沿边滑行、
默认姿态与 `judge_anchor` 判定一致。

**6b（WinEventHook 跟随）**：新增 `iuv-server/daemon/toolbar/mount_follow.rs`。

- hook 挂在**工具栏线程**（`WINEVENT_OUTOFCONTEXT` 回调由安装线程的消息循环派发）
  → **不引入定时器**，守住 40 号"无定时器、无前台查询"的显隐治理
- 事件范围 `EVENT_OBJECT_DESTROY(0x8001)`–`LOCATIONCHANGE(0x800B)` 一次覆盖四类，
  `idProcess` 限定目标进程；回调按 `id_object == OBJID_WINDOW` + hwnd 过滤
- **事件合并**：回调只投一发 `WM_APP_MOUNT_REFRESH`，`PENDING` 标志把一串
  LOCATIONCHANGE 压成一次（否则拖宠物时每帧一次 `SetWindowPos`）
- **隐藏 ≠ 解挂**：目标隐藏（Shimeji 会自己藏）→ 跳过本次定位、**保留 offset**，
  再现时自动恢复；只有 `DESTROY` / `IsWindow` 失效才被动解挂（工具栏留在原地）
- **拖拽 = offset 唯一写入口**：`end_drag` 里反推 offset + 重判锚点对 + 存档
- 新增依赖：windows crate 的 **`Win32_UI_Accessibility`** feature（`SetWinEventHook`
  所在模块；同 crate feature，非新依赖，已获批准）

**一个实现坑（已修）**：捕捉回调跑在捕捉线程上，**不能捕获 `self`**
（`ToolbarWindow` 含 `HWND`，非 `Send`）→ 只带走 `hwnd` 的裸值，回调内还原。

**Step 6 真机验收通过**（2026-10-14 15:32–15:33，零异常）：

- 启动吸附：档案里的宠物从 `@(2835,672)` 移到 `@(2818,598)` 仍命中，工具栏即刻归位
- **四次真实拖拽 → 四次姿态更新，`judge_anchor` 全部落在正确分支**：
  `[7,1]`（下方）/ `[8,6]`（右侧偏下）/ `[2,8]`（上方偏右）/ `[7,1]`
- 档案覆盖切换目标（BongoCat → WorkBuddy → Shimeji → BongoCat）五次全部
  `已保存` + `立即找回：命中` + `已挂上跟随`，hook 的 pid 正确跟着换
- 顺带验证挂**普通窗口**（`WorkBuddy.exe · Chrome_WidgetWin_1 · 1554×916`）也成立，
  且未误判为全屏覆盖（1554 < 1600）

### 2026-10-14 · Step 7 设置页「追踪宠物」标签页

`daemon/settings.rs` 加 `Tab::Pet`（标签顺序：常用/按键/外观/词库/游戏/**追踪宠物**/高级）：

- **当前挂载目标**卡片：进程 / 窗口类 / 尺寸提示（注明"仅供消歧"）/ 挂靠姿态 + **解锁**按钮
- 未挂载时显示引导（点瞄准镜 → 悬停 → 左键）
- **说明**卡片：跟随语义、被动解挂、隐藏态行为、档案路径
- 解锁 = `toolbar::unmount()`（清档案 + 卸 hook，工具栏留在原地）；可从设置页线程调用——
  状态由 Mutex/原子量保护，`UnhookWinEvent` 无线程亲和性，工具栏线程若正在 `refresh`
  取到 `None` 会自行早退
- 标签栏 `horizontal` → `horizontal_wrapped`：7 个标签后宽度余量不多，换行兜底胜过裁掉最后一个
