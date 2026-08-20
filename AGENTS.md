# iuv 输入法（代号 iuvim，谐音"哎哟喂"）

Rust + TSF 的 Windows 中文输入法。核心卖点（M2 起）：**用户掌控排序**——静态词频序默认稳定
（肌肉记忆安全）+ Shift+←/→ 主动调权（绝对值覆盖，反复调整收敛，见 `docs/plan/18-m2-user-dict.md`）。
M2（当前里程碑）：用户掌控排序——主动调权 + 用户词库/自造词/隐藏，已结案。

## 当前状态

- [x] M1 最小 MVP：全拼打字链路（见 `docs/plan/00-overview.md`）——**已结案**（2026-08-09：手测 1-8 项通过、词库缺失透明模式通过）
  - 已知问题：Alt+Tab 切窗口残留预编辑——**已修（2026-08-14）**：未确认输入按**原文上屏**语义结束
    （`zhujincheng` 上屏为 zhujincheng，非带撇号分节/汉字残留），与关闭输入法（Ctrl+Space）统一走
    `flush_session`（session.pending_text() → composition.commit → 清槽；空/失败降级 cancel）。
  - **已知 bug（2026-08-11，已修）**：续接（选中间级词）后尾巴 commit 失败 `0x8000FFFF (E_UNEXPECTED)`。
    根因：选中间词走「EndComposition 上屏已选词 → 紧接 StartComposition 重建尾巴」，重建的 composition
    被 TSF 在应用（notepad 实测）的下一个 edit session 里终止（日志 `composition 终止通知`），而
    `OnCompositionTerminated` 不清理 → 后续 GetRange/EndComposition 永久失败。
    **修复方案（悬空状态）**：选中间级词不再产生任何 commit 信号——`part_commit` 契约字段删除；
    已选词悬空入栈，预编辑混合显示（`床前ming'yue'guang`），composition 全程单个、只做 set_text 全量更新，
    End→Start 窗口不存在 → bug 不复现；Esc 语义改为有已选词时上屏已选词；`OnCompositionTerminated` 兜底
    清槽+置终止标志，TSF 侧检测后丢弃会话降级重建。改动：session.rs/key.rs（Effect 删 part_commit）、
    session_bridge.rs、composition.rs（sink 共享槽）、text_service.rs（降级）、测试/契约/文档同步。
- [x] M1.5 候选策略对齐微软（2026-08-12 落地；**2026-08-18 全拼两通道重写**）：**三路路由**——单段档
  （`c`/`sh`/`shi` 纯单字，首字母桶 `initial_top`）；多段全完整档（`nihao`/`xi'an`）**及末音节可补全**
  （`shigechengy`）→ **全拼两通道**：整句通道（`sentence_candidates`，词库负责"词"、Viterbi 只负责
  "唯一最佳句子"——2a 整串一次 / 2b 末段补全逐补齐一次取最高，至多一条 Sentence，**不再遍历每级/每
  切分方案组句**）+ 词条通道（k=n..1 砍末音节 exact，两路砍完第一刀后前缀对齐）；多段纯简拼档
  （`nh`/`nhm`/`nhmsx`
  构建期简拼键逐级砍尾巴，纯词、任意长度、部分消费尾巴续接复用悬空机制）；多段混拼档（`nhao`
  简拼段运行时展开音节笛卡尔配对，单级 ≤2000 查询剪枝）。数据层：dictc 对 ≥2 音节词生成简拼键
  （同表混存，路由隔离，IMEDIC01 格式零改动、新旧词库双向兼容）+ `Dict::initial_top` 首字母桶
  （每字母 top-500 词频序）。依据：`docs/research/msime-probe-checklist.txt` 微软实测清单
  （A~H 全组）。已知差距（M3+）：符号/emoji 候选、学习候选（微软 Z 键与学习词条；
  候选翻页/每页 5 个/翻页键自定义已对齐——见 2cc189b/d1dcfb8/8f479f9）；排序用白霜
  词频与微软有数据级差异（M2 主动调权+自造词已部分缓解）。
- [x] M1.6 IMEDIC02 平面词库 + mmap 零加工加载（2026-08-13 落地，已并入 main）：
  **引擎冷加载 2.1s → 70ms**（真词库 125.5 万条实测）。dictc 编译期固化排序/索引/首字母桶/音节表
  为段表驱动平面格式；加载 = mmap + 段定位 + 边界校验扫描，零分配零重建；物理内存全系统一份
  （页缓存共享），新开任意软件首键即进拼音。查询返回物化 `Vec<Entry>`（mmap 无法零拷贝借用）。
  决策：校验只做简单边界检查（不查排序不变量）；**IMEDIC01 读路径已删**（老词库需重编译）。
  `Dict::from_entries` 签名保留（内部走序列化→解析统一路径），测试零改动。
- [x] 无匹配输入原文兜底（2026-08-14 落地）：`input`/`window`/`i` 等任何路由都打不到词库的输入，
  `generate_candidates` 末尾补一条原文候选（去 `'`、seg_len 全消费、1/Space 直接上屏），
  候选窗内容恒非空——修复英文输入时 2mm×1mm 全空候选框（根因：候选为空 + reading 非空，
  双空守卫放行 → layout 空 items → 16×8px 窗口）。候选窗对原文候选**不编号**呈现
  （text == 预编辑原文去 `'` 即判定，gdi.rs），传达"不认识"语义。
- [x] 大写保形进序列（2026-08-14 落地）：Shift/CapsLock 字母 → `Key::ShiftChar` 大写原样进 raw
  （匹配只认小写：大写不被音节表命中、按不可匹配字符处理，`niHAO` 候选仍从 `ni` 前缀出；
  commit 原样上屏 `niHAO`/`Hello`）；字母大小写 = Shift 与 CapsLock 的 XOR（CapsLock+Shift 反转小写）；
  **大写同样是开会话键**（`is_session_start_key` 字母即开会话，`Hello` 的 H 进序列而非直接上屏）。
  **CapsLock 例外（同日追加）**：Caps 生效时会话外字母放行直通（仿微软 Caps=英文模式，
  不建会话→游戏无预编辑/候选窗；会话内 Caps 字母照常进序列，避免 composition 残留），
  Shift 单独大写不受影响——`session_bridge::caps_passthrough`。改动：key.rs/session.rs
  （ShiftChar 臂）、keymap.rs（is_session_start_key 单条件）、session_bridge.rs（map_key XOR + caps_passthrough）、
  text_service.rs（capslock_on 传参）。
- [x] **按键直通白名单（2026-08-14 待测）**：`config.json` 新字段 `passthrough_apps`（exe 名列表，
  大小写不敏感精确匹配，仿 Weasel PR #1049），命中进程 TSF 层**全部按键放行**——不建会话、
  无候选窗/预编辑，输入法在该进程完全透明（游戏 WASD 直达，与 Caps 直通正交互补：Caps 管
  "Caps 状态"、白名单管"特定进程"）。名单为空零开销（不查进程名）；判定在 handle_key_down
  最前部（english_mode 检查后）——`session_bridge::is_passthrough_app` + `log::module_name` 复用。
  改动：iuv-core config（字段+测试）、iuv-tsf（is_passthrough_app/log 公开/判定）、契约/任务书同步。
  边界：名单进程无法中文输入；config 改后需重载输入法（热重载不做）。
- [x] **M2 主动调权 + 用户词库（2026-08-14 已结案：手测通过、已并入 main）**：Shift+←/→ 与页内相邻
  候选**交换权重**（立即重排、高亮跟随、不关会话、边界忽略）；持久化为**绝对值覆盖**
  （互写对方合成权重，无 delta 魔法数字——反复调整收敛，排序决定权交还用户，替代滞回
  自动换位：滞回只防短期抖动、治不了长期漂移击穿肌肉记忆，降级为可选细节）；用户库
  `iuv.user.imedic`（IUVUSR01 线性格式，覆盖表内存 BTreeMap + 写时复制）与基本库查询时
  **叠加**（merge 下沉 Dict 查询层，引擎算法零改动；基本库 mmap 只读不动）；跨进程
  **会话级 mtime 重载**延迟生效；写盘 = 临时文件 + 先删后 rename 原子替换，失败不阻断
  （内存态已生效）；TSF 键位选 Shift（**Alt 组合 = WM_SYSKEYDOWN 不进 TSF 键 sink，
  机制死路**——快捷键设计红线，见 18-m2-user-dict.md 附录；Ctrl 冲突大保持放行）。
  改动：iuv-data（userdict.rs 新增、dict.rs merge）、iuv-core
  （key.rs SwapLeft/SwapRight、engine.rs attach/swap/mtime 重载、session.rs Swap 臂）、
  iuv-tsf（map_key、load_engine 装配、按键日志）。测试：数据层 5 + 引擎 8 + map_key 2 全绿。
- [x] **M2 自造词 + 隐藏（2026-08-14 已结案：手测通过、已并入 main）**：逐字选择（picked 全单字、≥2 字、
  全消费 commit）记录为自造词——场景 0（词库已有整词 → 跳过）/ a（无命中 → 权重 8000）/
  b（n 条命中 → 目标词位 = 首页最后：n≥page_size → avg(第 ps-1, 第 ps 位)、否则第 n 位减一，
  page_size 为变量非 magic）；自造词与覆盖统一存用户库段1（IUVUSR02 升级：+屏蔽段，
  magic 分派兼容读 01 旧文件），**Dict::merged 追加用户库独有条目**（词不在基本库组 →
  随查询显示，viterbi 整句同吃到）；Shift+Delete 隐藏——先删用户库条目（撤销自造），
  否则屏蔽基础库词条（**viterbi 整句同样拦截**，否则隐藏"手癣"后整句仍被组出）；
  裸 Delete 放行给应用。改动：iuv-data（userdict.rs 02 格式/block/remove_entry、
  dict.rs merged 三叠加 + exact_raw）、iuv-core（key.rs HideCandidate、engine.rs
  record_phrase/hide_entry/install_user、session.rs commit 判定 + Hide 臂 + Sentence
  屏蔽拦截）、iuv-tsf（map_key Shift+Delete）。测试：数据层 5 + 引擎 4 + 会话 7 全绿。
- 点子库（暂不做，2026-08-19 记录）：**Tab 键用途**——整句翻译（在线 API：空闲 0.5s 触发、候选 N+1 槽、
  Tab 高亮 + 空格上屏）与自动补全（Tab 钉选当前候选续打，不结束会话），语义分配未定，`29-tab-ideas.md`
- **M9 可自定义贴图皮肤框架——调研定稿/挂起（2026-08-20，未实现）**：候选窗换肤 = 自研 `IUVSKIN01`
  （`skins/<name>/manifest.json` + 多区域 PNG，9-patch 缩放，部分贴图渐进增强，加载失败降级 light/dark），
  零新增依赖（tiny-skia 默认 `png-format` + `draw_pixmap` 缩放已确认）。**Lua 插件兼容已否决**（调研实测：
  librime 不内置 Lua、Weasel 默认不带、全 GitHub 用户级 Lua 插件仅 ~4 个合计 <100 星——`33-skin.md` §1）。
  皮肤格式互操作合法（红线：不抄搜狗/QQ 解析代码；只做自研格式）。**挂起原因**：前置 M8 悬浮工具栏
  （feat-toolbar 分支，效果差）需先改进。`33-skin.md`
- 后续：M3 整句增强(LMDG)/模糊音 · **M4 跨平台渲染候选窗——已实现（2026-08-16，ui-rewrite 分支待手测）**：
  tiny-skia+cosmic-text 绘图（crates/iuv-ui）+ D2D/DComp 呈现（ui/candwin.rs）+ 浅色/深色主题 + 圆角阴影，`19-m4-cross-render.md`
  · **M5 语言栏右键菜单——已实现（2026-08-17 重定义，去托盘）**：右键语言栏「中/英」按钮弹「设置/关于」
  （TSF InitMenu/ITfMenu 官方机制），`21-m5-tray-menu.md`
  · **M6 守护进程——已实现（2026-08-16，2026-08-17 修正）**：iuv-daemon exe 唯一持有用户库（共享段 + 命名管道 IPC +
  egui 设置页主线程），会话进程 daemon_client（共享段只读引用 + 写走管道 + 离线降级本地 + config 热载），`22-m6-daemon.md`
  · **M7 安装器/词库导入/x86（daemon 首会话自启已实现——2026-08-17：Activate 检测离线 → 60s 节流 →
    CreateProcessW 拉起 DLL 同目录 iuv-daemon.exe，搜狗同款惰性拉起；dev-deploy 已部署 daemon；键位热载仍待）**
  - **钉选不做**（2026-08-14 用户决策）：Shift+←/→ 手动排序 + 增/删自定义已满足，显式"锁死"交互取消
  - **Tauri 已废**（2026-08-16 用户决策）：M4 不做 WebView helper；候选窗/菜单用 iuv-ui 自绘（tiny-skia），设置页 M6 用 egui/eframe
  - **无独立托盘图标**（2026-08-17 用户决策）：右键菜单挂语言栏「中/英」按钮（TSF InitMenu），托盘/自绘菜单窗口已删；
    daemon 纯后台（无图标），设置页入口 = 语言栏菜单 → 管道 OpenSettings
  - **M4~M6 待手测项**（2026-08-16 完成后未验收）：M4 真透明圆角/阴影/深色主题/不抢焦点/多显示器 DPI（2026-08-17
    已修 BeginDraw 关联 bug，候选窗此前不可见）；M5 语言栏右键菜单两项；M6 双进程即时一致/守护杀死降级/设置页热载
- [x] **设置-常用 = 新 TSF 实例初始状态（28-initial-state-settings.md，2026-08-19 落地，待手测）**：
  「常用」页四组开关（中/英、半角/全角、简/繁、标点）+ 每页候选数下拉 [5,6,7,8,9]，存
  `config.json` 新父节点 `initial_state`（全部 lowercase 枚举：`mode`/`width`/`script`/`punct`，
  复用 iuv-core 类型，daemon 已加 iuv-core 依赖）。中/英默认每次 Activate 强制写 OPENCLOSE
   compartment（中文默认 = 旧「激活即打开」零变化；英文默认 = 新实例从英文起）；**半角/全角已生效**
（2026-08-19：会话外全角转换，见下条）、简体/繁体已生效（见下条）；标点判定读 `initial_state.punct`。**旧顶层
   `english_punctuation: bool` 迁移**：iuv-core from_file 与 daemon load 双向 shim（bool→枚举），
   save 时清理旧键。默认 = 主流（中文/半角/简体/中文标点）。改动：iuv-core config（+4 枚举/结构体/
   迁移 shim/导出）、iuv-tsf（Activate 默认模式 + 标点判定）、iuv-daemon（config.rs 签名重构
   `save_config(&DaemonConfig)` + settings 常用页重排 + page_size 钳制 5..=9）、脚本模板、契约/文档同步。
   测试：iuv-core 迁移/默认 5 + daemon load/save/迁移/钳制 6 全绿。
- [x] **自绘候选窗抑制改名单驱动（2026-08-20 已修，待手测）**：微信打字 `ceshi` 到第 4 键候选栏消失
  ——根因是 wow-ime 的 `ImmDetect` 按 GetTextExt 退化矩形（w/h≤2 连续 3 次）自动判 IMM 客户端并抑制
  自绘候选窗，微信编辑器对折叠 composition range 返回 2×1 薄光标（日志实测：首字母 14×16 真矩形、
  此后每键 2×1；位置逐键右移是真实光标仅尺寸小）→ 第 3 键即误判抑制，而微信不自绘候选栏（不像
  WoW 走 TSF→IMM 桥），候选整个消失。**修复**：删 `ImmDetect` 矩形启发式，改 `config.json` 新字段
  `candidate_owner_apps`（exe 名单，同 `passthrough_apps` 匹配语义）驱动 `set_suppressed`——命中进程
  （如 WoW 自绘游戏内候选栏）才抑制，**默认空 = 恒自绘（微信自动修复）**；候选 UI 元素同步不受抑制
  影响（游戏桥仍可拉候选）。**安装脚本默认预置 `wow.exe`**（install/dev-deploy 模板，2026-08-20 追加；
  代码层 `Config::default()` 仍为 `[]` 兜底恒自绘），其他 exe 用户自行追加（**设置页-高级-候选自绘应用
  输入框可改**，daemon `DaemonConfig` 已收编该字段）。改动：iuv-core config（字段+测试）、
  iuv-tsf（text_service.rs 删 ImmDetect/
  dispatch_effect 名单判定 + candwin 翻转日志/注释）、daemon（settings.rs LOG_MODULES 删 immdetect）、
  契约/26/install 模板/AGENTS 同步。测试：iuv-core 1 + iuv-tsf 1 新增全绿。
- [x] **全角行为（2026-08-19 落地，待手测）**：`initial_state.width == Full` 时**会话外直通路径**套
  `fullwidth` 转换——**中文模式**数字 `0-9`→`０-９`、标点表未收符号（`/` `_`）→全角形、空格→`U+3000`，
  标点表内符号仍归标点开关、字母照常进拼音会话；**英文模式**字母（大小写=Shift⊕Caps）/数字/符号/空格
  全转（`ｍｉｃｒｏｓｏｆｔ１２３`，微软实测对齐）；拼音会话内不转换、白名单进程优先、Ctrl/Alt 放行。
  改动：iuv-core（punct.rs `fullwidth`，`0x21..=0x7E` 一律 +0xFEE0 无例外）、iuv-tsf
  （session_bridge.rs `fullwidth_pending` 纯函数 + text_service handle/test_key_down 对称接线，白名单
  提到最前防覆盖透明性）、iuv-daemon settings 提示文案、契约/28/AGENTS 同步。测试：iuv-core 映射 5 +
  iuv-tsf 决策 4 全绿。运行时 Shift+Space 切换热键**不做**（2026-08-19 用户决策）。
  **补充（同日）：预编辑原文上屏转全角**——Enter/无候选空格/flush/原文兜底候选提交的拼音原文
  全角下输出全角（`nihao`→`ｎｉｈａｏ`），候选（汉字）不受影响、自造词录原文不录全角；
  实现：session `to_output` 套 `punct::fullwidth_text`，`all_text()`/`commit_index` 一处覆盖 TSF 零改动。
  测试：iuv-core 映射 1 + 集成 3（全角 Enter/兜底、半角回归）全绿。
- [x] **简体/繁体切换（31-script-traditional.md，2026-08-19 已结案：手测通过、并入 main）**：
  `initial_state.script == "traditional"` → **繁体模式 = 简体词库 + 运行时简→繁转换**（s2t 通用繁体）：
  候选/预编辑/上屏显示繁体、内部词库/自造词/调权/屏蔽恒简体（同全角「录原文不录全角」）。
  数据 = **形态3 数据文件 `iuv.opencc`**（2026-08-19 用户拍板：不做编进 DLL、不做 daemon IPC——转换在
  热路径、daemon 是 Windows-only，与 iuv.imedic 词库管线同构跨平台）：`scripts/download-opencc.ps1`
  拉取 OpenCC 数据（BYVoid，**Apache-2.0**，入 `data/opencc/` gitignore）→ dictc 新子命令
  `dictc opencc` 编译成 **IUVOCC01** 二进制 → install/dev-deploy 复制到 `%LOCALAPPDATA%\iuv\iuv.opencc`
  （Replace-InUseFile 同款 mmap 锁）。转换 = 正向最长匹配（短语表优先、单字兜底、未命中直通幂等）；
  已知差距：单字一简多繁取首值无上下文模型（`后→后`、`发→发`）。挂点与全角同构：`to_output`
  fullwidth 后 + `convert_script`；`effect()` 显示边界转 composition/reading/candidates/all_candidates。
  装配：Engine `attach_script_converter`；数据缺失/损坏 → None 降级简体不崩。改动：iuv-data
  （opencc.rs + dictc opencc + 导出）、iuv-core（script.rs ScriptConverter + engine 字段 + session 挂点）、
  iuv-tsf（load_engine 装配 + script_path）、iuv-daemon（settings 文案已生效）、scripts、契约/02/AGENTS 同步。
  测试：iuv-data 9 + script 2 + 会话集成 6（繁体候选/单字/整词上屏/自造词录简体/简体回归/降级）全绿。
- **中英切换已改系统机制（2026-08-12）**：`OPENCLOSE` compartment 真相源（系统"输入法/非输入法切换"热键驱动，
  OnChange 统一响应；语言栏点击归一写 compartment；Shift 切换已移除；**激活初值 = config `initial_state.mode`**，
  中文默认 = 激活即打开）。前置条件：用户在
  高级键设置把"输入法/非输入法切换"设为 Ctrl+Space（"切换输入语言"热键让位，Win+Space 仍可用）。
  已知遗留（已修 2026-08-14）：有活动候选时按热键关闭，未确认输入按原文上屏（原 bug：
  只清内存态不终止 composition → 带撇号分节预览残留；Alt+Tab 同根因一并修复）。

## 开发入口

**先读 `docs/plan/01-contract.md`（共享契约，接口唯一权威来源）**，再读对应模块任务书。
执行流程（W0 骨架 → W1 并行实现 → W2 组装）见 `docs/plan/00-overview.md` §3 与 `20-assembly.md`。

## 结构

| 路径 | 说明 |
|---|---|
| `crates/iuv-data` | 词库编译器 dictc + 二进制格式 + Dict 查询层 + 用户库（跨平台） |
| `crates/iuv-core` | 引擎：切分/候选生成/unigram Viterbi/会话状态机/排序管线（跨平台纯 Rust） |
| `crates/iuv-ui` | 候选窗/菜单绘图层：tiny-skia + cosmic-text + Theme（跨平台纯 Rust，M4 已实现） |
| `crates/iuv-repl` | CLI 调试前端（跨平台） |
| `platforms/windows/iuv-tsf` | cdylib：COM/TSF 管线 + 候选窗窗口层（ULW 呈现）+ 语言栏"中/英"切换图标/右键菜单（Windows） |
| `platforms/windows/iuv-win` | Windows 共享层：ULW 呈现（`ulw.rs`）+ 自绘弹窗骨架（`popup.rs` LayeredWindow）+ M6 管道 IPC/共享段（`ipc/`+`shm.rs`，2026-08-21 自 iuv-data 移入） |
| `platforms/windows/iuv-daemon` | 守护进程 exe：唯一持有用户库（共享段+管道 IPC）+ egui 设置页（M6 已实现，纯后台无图标） |
| `platforms/{macos,linux}/` | 占位：IMK / Fcitx5·IBus 适配层 + 门面规划（README，见各目录） |
| `data/` | 下载的词库（gitignore；白霜拼音 GPL-3.0，不入库） |
| `scripts/` | download-dict / install / uninstall / dev-deploy（热部署） / iuv-common（共享库：提权/日志/ctfmon/延迟清理/Replace-InUseDll） |

## 常用命令

```powershell
cargo check --workspace
cargo test --workspace
cargo build -p iuv-tsf --release
scripts\download-dict.ps1
scripts\install.ps1        # 安装（管理员，自动弹 UAC）
scripts\uninstall.ps1      # 卸载（管理员，自动弹 UAC）
scripts\dev-deploy.ps1     # 热部署：改完代码后免注销生效（默认先构建；-SkipBuild 跳过）
```

## 硬性约定

- 依赖白名单制（`docs/plan/01-contract.md` §2），新增 crate 需主智能体批准
- 文件属主矩阵（`docs/plan/01-contract.md` §6）：并行开发只改自己属主的文件
- iuv-core 保持跨平台纯 Rust；iuv-tsf 内绝不 panic 到宿主进程；测试纪律见 `docs/plan/30-conventions.md`
