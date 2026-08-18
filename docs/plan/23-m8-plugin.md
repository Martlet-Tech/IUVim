# 23 · 任务书 M8：i/u/v 功能键插件系统（命令调用模型）

> 状态：**设计定案**（2026-08-18 决策冻结，未写代码）。
> 前置阅读：`00-overview.md`、`01-contract.md`（§4 候选契约）、`16-command-mode.md`（iuv 命令体系构想）、
> `22-m6-daemon.md`（管道 IPC 网关先例）。
> 决策记录：见 §2（2026-08-18 四轮问答收敛）。

## 1. 目标（验收一句话）

i/u/v 三个**不可能作汉字拼音首字母**的键（`i`→yi、`u`→wu、`v`→ü）开放为**可分配功能槽**：
插件 = 用户插件目录里的一个子文件夹 + 合法 `manifest.json`，设置页三个下拉（i/u/v）列出全部
合法插件 `name`，用户选择即绑定（改映射即覆盖，无需删除内置）。插件 = 普通命令调用
（`<cmd> <prefix> <input> [TSF上下文]` → stdout JSON），语言无关（内置三插件用 Python/JS/Rust
三语言各写一个，dogfooding 验证协议），经守护进程转发执行，候选并入现有管线。

## 2. 背景与决策记录（2026-08-18）

- 出发点：微软/QQ/搜狗都有 i/u/v 特殊功能（i=表情、u=拆字、v=数字/日期），但**写死**。
  iuv 差异化 = 功能键插件化：可重分配 + 用户自写模块覆盖内置。此构想源自 `16-command-mode.md`。
- **键位可行性**：i/u/v 单段 → `Route::Empty`（engine.rs classify 拦截），现状走原文兜底，
  接入功能模式零冲突。
- 四轮决策：
  1. **运行时模型**：子进程；**API 边界围绕"键盘键值进、文本出到输入框"**；**完全统一：
     内置也是插件**；**match 门控**防英文劫持（`vip`/`iPod`/`URL` 不匹配 → 放行正常路径）。
  2. **语言之争**（Lua vs Python）：Lua 资源低但能力需我们预定义；Python 全权但需绑运行时。
     定案：**不绑任何运行时**，只提供命令调用（exe/.py/.lua 用户自己搞）；TSF 上下文
     （窗口名/光标位置等）走**调用参数**传过去；捕捉 stdout 返回值；**每次预编辑变更产生一次新调用**。
  3. **内置三插件**：由本仓库维护，故意用三种语言：Python=拼音→emoji（`iaixin`→❤）、
     JS=数字渲染（`2002.1.1`→年月日汉字、`10003yuan`→10003元/一万〇三元）、Rust exe=拼字
     （`木木木`→森、`水水水`→淼）。stdout **统一 JSON**（可嵌 base64，可玩性更高）；spawn **经 daemon 转发**。
  4. **目录发现 + manifest + 下拉绑定**：插件放**用户目录**
     `%LOCALAPPDATA%\iuv\plugins`（一目录制，内置也播种其中），每个子文件夹 = 一个插件，
     必须含 `manifest.json`；**文件夹名即身份**（魔兽世界同款：作者不写 UUID、无 id 分配器），
     设置页三个下拉列出合法插件 `name`，**绑定 = key → 文件夹名**，改名 → 设置页提示重选；
     **覆盖 = 下拉重分配**（用户放新插件选新的即是，删除式覆盖任用户）；下拉含**"禁用"空选项**。
  5. **错误通道**：四类失败（exe 不存在 / 非零退出 / 超时 / stdout 非 JSON）全在 daemon spawn 层
     可捕获（Windows Store 别名桩靠**非零退出码**兜底，见 §6.1）；per-plugin `last_error` +
     `PluginResult.error` 回传不吞掉；候选窗显示**两行无号码**（原文兜底 + ⛔ 状态行，见 §6.3），
     设置页列详情。

## 3. 架构与数据流

```
按键流 → iuv-core（session/function_keys 路由）
       → iuv-tsf daemon_client（管道 IPC：PluginQuery 请求，携带 TSF 上下文）
       → iuv-daemon（插件网关：扫目录/读 manifest → spawn <cmd> → 超时/异步 → 捕获 stdout JSON）
       → daemon 回 PluginResult（候选/直通/放行）→ iuv-core 并入候选管线 → 候选窗
```

- **路由主表** = `config.function_keys`（键 → 插件**文件夹名**）；插件 manifest 的 `match`
  做**预门控**（次路由，省 spawn，见 §6），空输出 pass 兜底防劫持。
- **执行在 daemon**：集中管理、TSF 不碰子进程逻辑；daemon 离线 → 三键降级普通键（原文兜底）
  + 候选窗明确提示，绝不挂键。
- **不经 daemon 的路径不存在**：插件查询全部走管道（含内置插件）。

## 4. 插件目录与 manifest（动态加载/替换的依据）

- 插件目录：`%LOCALAPPDATA%\iuv\plugins`（与模板 `templates.json` 同根，符合既有惯例）。
- 一目录制：内置插件由 dev-deploy/安装器**播种进用户目录**，与用户插件同等待遇
  （可看/改/删；重装会覆盖用户对内置的修改，文档注明）。
- **文件夹名即身份**：绑定、state 文件、下拉绑定全用文件夹名。复制文件夹 = 新插件。
  内置文件夹带前缀防撞车：`builtin-emoji` / `builtin-number` / `builtin-decompose`。
- **热替换天然免费**：命令模型每次按键重新 spawn，改文件即下键生效、无缓存无重启；
  只有 manifest 变更影响列表与绑定。daemon 打开设置页/配置重载时重扫目录
  （dir mtime 变更也触发重扫）。
- **合法判定**：`manifest.json` 可解析 + 必填字段非空 → 进插件列表；否则设置页**灰显 + 原因**
  （避免"我放了文件夹怎么没出现"）。

### 4.1 manifest.json 规格

```json
{
  "name": "数字渲染",
  "version": "1.0.0",
  "command": ["node", "number.js"],
  "description": "v2002.1.1 → 年月日汉字；v10003yuan → 一万〇三元",
  "author": "iuv 团队",
  "homepage": "https://…",
  "timeout_ms": 200,
  "match": ["^v\\d"],
  "icon": "icon.png",
  "requires": "需 Node 18+",
  "format": "json"
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `name` | ✓ | 下拉显示名；重复时显示 `name (文件夹名)` 消歧 |
| `version` | ✓ | 版本号（semver），设置页展示 |
| `command` | ✓ | 数组 `["node","number.js"]` 或字符串；**相对路径相对插件文件夹**，win 上裸 exe 名即可 |
| `description` | 选 | 下拉悬停/详情展示 |
| `author` / `homepage` | 选 | 元数据，M9 市场用 |
| `timeout_ms` | 选 | 覆盖默认 200ms |
| `match` | 选 | 激活模式（正则数组），预门控省 spawn；不填则一律调用（空输出=pass 兜底） |
| `icon` | 选 | 图标路径，设置页列表/候选窗品牌化 |
| `requires` | 选 | 运行时依赖提示文本，插件运行失败时设置页亮出（builtin.number 的 node 依赖靠它） |
| `format` | 保留 | 默认 `"json"`，留扩展位 |

> **不做 id 字段**（v1）：文件夹名即身份，作者零负担。M9 插件市场若需跨目录稳定身份再议，
> 由 `format` 扩展位演进。

### 4.2 配置（config.json）

```json
"function_keys": { "i": "builtin-emoji", "u": "builtin-decompose", "v": "builtin-number" }
```

- 删除早期设想的 `plugins` 表：发现全走目录扫描，config 只存绑定。
- 值 = 文件夹名；绑定目标已不存在（文件夹被删/改名）→ 该键按普通键处理，
  设置页下拉显示"未找到，请重选"。

## 5. 调用约定（每次预编辑变更 = 一次调用）

```
<cmd> <prefix> <input> [--window <窗口标题>] [--class <窗口类>] [--hwnd <hex>]
      [--caret-x <px>] [--caret-y <px>] [--state <插件专属状态文件>]
```

- `prefix` = 功能键字符（i/u/v）；`input` = 其后内容（如 `v 2019.1.1`）。
- 工作目录 = 插件文件夹，相对路径可用。
- `--state`：`%LOCALAPPDATA%\iuv\plugin-state\<文件夹名>`（插件目录**外**，替换文件夹不丢数据），
  插件自己读写，我们不管理 IO。

## 6. stdout JSON 契约（统一 JSON）

```json
{ "cands":  [ {"text":"❤","cm":"爱心","icon":"<base64 png>","w":16,"h":16} ] }  // 候选
{ "commit": "一万〇三元" }                                                        // 直通上屏
{ "pass": true }   // 或空 cands → 放行（英文防劫持天然成立）
```

- `cm` = 候选右上标备注；`icon` 嵌 base64 自定义图标/图片（可玩性扩展，M8b 渲染接入）。
- **exit 0 + `pass`/空输出 = 真·无匹配**（`vip` 场景，静默放行是对的）；错误语义见 §6.1。
- 超时默认 200ms（manifest `timeout_ms` 覆盖）；kill 僵尸进程。

### 6.1 失败分类语义（daemon spawn 层全可观测，不吞）

| 失败 | 捕获 | 语义 |
|---|---|---|
| exe 不存在（PATH 无 python/node） | `spawn()` `Err(NotFound)` | **错误**：报错 |
| 非零退出 | wait 拿 exit code ≠ 0 + 捕捉 stderr | **错误**：报错 |
| 超时 | 200ms kill | **错误**：报错 |
| stdout 非 JSON | 解析失败 | **错误**：报错 |
| exit 0 + `pass`/空输出 | — | **真·无匹配**：静默放行（`vip` 场景） |

> ⚠️ **Windows 坑**：无 python 的机器上 `python` 可能解析到 Microsoft Store App Execution
> Alias 桩（`WindowsApps\python.exe`）——`spawn()` 成功但秒退、退出码 9009、stderr 空。
> 故不能只靠 NotFound 判定，**非零退出码是必选兜底**；文案基于退出码 + `requires` 文本。

### 6.2 错误通道（PluginResult.error）

- daemon 维护 per-plugin `last_error {kind, exit_code, stderr(截断 4KB), ts}`，每次调用刷新；
  该插件下一次成功运行即清除。
- `PluginResult` 响应带 `error` 字段回传 TSF，**不吞掉**（现状日志照记）。
- 展示 = 静态 `requires`（manifest）+ 动态实际错误（kind 中文文案 + stderr 片段）合并。

### 6.3 候选窗错误状态行（呈现定案）

绑定键处于插件错误态时，候选窗显示**两行、均无号码**：

```
预编辑: imumumu
┌──────────────────────────┐
│ 1  imumumu              │   ← 无号码（text==原文兜底，可 1/Space 上屏，现有"不认识"语义）
│    ⛔ 找不到 node        │   ← 无号码、不可上屏，纯状态行（可附 stderr/requires）
└──────────────────────────┘
```

- 出现时机：仅错误态；正常 pass（`vip`）不出现，维持现状。
- 不可上屏：选择/1/空格对它无操作，不参与翻页计数与 Swap；Esc/退格照常。
- 清除：该插件下一次成功运行即清错误态。
- 实现落点：iuv-core 候选管线新增**状态行**概念（独立标记，非普通候选），iuv-ui 渲染为
  无号码警示行（⛔ + 主题警示色），M8b 改动。

## 7. 性能对策（逐键 spawn 的硬伤，正面处理）

1. **异步执行**：spawn 放后台线程，打字永不阻塞——按键即时进预编辑，候选到了再刷。
2. **match 预门控**：manifest 声明激活模式，明显不匹配（`vip` 对 `^v\d`）不 spawn 直接放行。
3. **LRU 缓存**：key = `prefix+input+window`，高频重复（`v3` 等）第二次起命中缓存，近零延迟。
4. **超时**：默认 200ms，超时 kill + 本次降级 pass。

## 8. 三个内置插件（本仓库维护，dogfooding 三语言）

| 插件文件夹 | 语言 | 运行时假设 | 功能 |
|---|---|---|---|
| `builtin-emoji` | Python | 需 python3 | `iaixin` → ❤（拼音→emoji 映射表，脚本内置） |
| `builtin-number` | JS | 需 node（npm） | `2002.1.1`→年/月/日汉字补全；`10003yuan`→10003元/一万〇三元 |
| `builtin-decompose` | Rust exe | 无 | `木木木`→森、`水水水`→淼（部件序列→合体字映射表） |

- 目录结构：仓库 `plugins/{builtin-emoji,builtin-number,builtin-decompose}/`，播种到用户目录。
- **依赖声明**：builtin-number 依赖用户 node；没有 node 时 v 键降级——恰好检验
  "插件依赖作者运行时"的报错体验（候选窗/设置页用 `requires` 亮提示）。

## 9. 设置页（daemon，egui）插件管理

- **三个下拉**（i/u/v）：内容 = 用户插件目录下**合法 manifest** 插件的 `name`
  （重复 name 显示 `name (文件夹名)`）+ **"禁用"空选项**（让该键恢复普通行为）。
- 选择写回 `config.function_keys`（存文件夹名）；"禁用"→ 清空该键绑定。
- 绑定目标已不存在 → 下拉显示"未找到，请重选" + 该键按普通键处理。
- 列表区：每个插件显示 name/version/author/`requires`；灰显非法 manifest 并给原因；
  无 node 等运行时失败的插件在选中时亮 `requires` 提示。

## 10. 部署

- 调试期：`scripts/dev-deploy.ps1` 扩展——播种 `plugins/` 三子目录到用户插件目录 +
  写默认 `function_keys` 映射。
- exe 分发：安装器（M7）负责播种。

## 11. 任务清单

| # | 任务 | 状态 |
|---|---|---|
| M8a | daemon 插件网关：目录扫描 + manifest 解析/校验 + 管道协议扩展（Request::PluginQuery/Response::PluginResult + error 字段，tag 0x08/0x09）+ spawn/超时/异步/缓存 + per-plugin `last_error` 记录 + iuv-core 路由接入（function_keys → 查询 → 候选并入） | 待做 |
| M8b | TSF 上下文采集（窗口标题/类/hwnd/光标坐标）传参 + 候选管线接入（`CandidateKind::Plugin`、翻页/高亮/Esc 复用、Swap 豁免、Shift+Delete 隐藏保留、不污染用户词库）+ **状态行概念（无号码/不可上屏/不参与翻页与 Swap）** + iuv-ui 候选 icon（base64）与 ⛔ 状态行渲染 | 待做 |
| M8c | 三个内置插件（builtin-emoji/builtin-number/builtin-decompose，**含 `requires` 字段**）+ dev-deploy 播种扩展 + 设置页三下拉/列表/禁用项/**错误状态区（last_error + requires）** + 示例插件 + 文档 | 待做 |

## 12. 已知风险与取舍

- **拆字数据源**：部件→合体字映射表无现成开源数据，需自建（几百常见合体字可半自动构建：
  木木木=森、水水水=淼、人从众、日日月=晶…）。许可证注意（同白霜词库 GPL 处理惯例）。
- **emoji 彩色渲染**：候选窗 tiny-skia/cosmic-text 对彩色 emoji 支持存疑；❤ 是文本可降级，
  真彩图走 `icon` base64 路径（M8b spike）。
- **JS 运行时假设**：builtin-number 依赖 node；无 node → v 键报错（候选窗 ⛔ 状态行 + 设置页详情），
  报错体验本身是要验证的产品点（§6.2/§6.3）。
- **daemon 依赖**：插件查询全走 daemon，离线时三键全失效 → 降级普通键 + 明确提示（承接 22 号任务书降级纪律）。
- **语言无关的代价**：命令调用无沙箱，插件以用户全权限运行（用户已接受："用户自己搞吧"）。
- **文件夹名即身份**：改名/删文件夹即断绑定 → 设置页"未找到，请重选"覆盖，接受（WoW 同款取舍）。

## 13. 槽位

- M9：插件市场/注册表（iuvpm）；若需跨目录稳定身份再议 `id` 字段。
- Lua runtime：协议留好，若需 Rime 生态互操作/小体积，manifest 加 `runtime:"lua"` 字段即可。
- 交互式/多步插件（日期选择器类 UI）：二期，需自定义渲染，v1 不做。

## 14. DoD（未实现，实施后填）

```
cargo check --workspace / cargo test --workspace      # 全绿
手测：用户目录放插件文件夹 + manifest → 设置页三下拉出现；
i/u/v 三键出内置候选、翻页/高亮/Esc；vip/iPod 不误伤（英文放行）；
下拉改选/禁用即时生效、改名后"未找到，请重选"；daemon 杀死后三键降级不挂；emoji 候选渲染
无 python/node 机器：i/u/v 键显示"原文兜底 + ⛔ 状态行"两行无号码、不可上屏、不挂键；设置页列错误详情
```
