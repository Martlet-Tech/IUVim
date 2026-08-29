# 工作状态台账

> 本文件 = 完整工作台账：每项落地的根因/方案/改动/测试记录。新条目追加到文末。
> AGENTS.md 只保留导航与活跃事项速览并指向本文件；细节权威来源 = git log 与
> `docs/plan/` 任务书。结案条目不删除（历史档案）。
> **提交约定：允许提交 = 已测试通过**——台账不设也不回溯维护「待手测」状态。

## 活跃事项速览

### 未开工 / 挂起

- M3 整句增强(LMDG)/模糊音
- 符号/emoji 候选、学习候选（微软对齐已知差距，见 M1.5 条目）
- M7 键位热载（config 改后需重载输入法）
- M9 可自定义贴图皮肤框架（调研定稿/挂起；前置 M8 工具栏已多轮打磨，可重新评估）
- 点子库：Tab 键用途（`29-tab-ideas.md`，暂不做）

---

## 台账

- [x] M1 最小 MVP：全拼打字链路（见 `docs/plan/00-overview.md`）——**已结案**（2026-08-09：手测 1-8 项通过、词库缺失透明模式通过）
  - 已知问题：Alt+Tab 切窗口残留预编辑——**原已修（2026-08-14），2026-08-21 设计变更见下**：旧语义=未确认输入按**原文上屏**结束
    （`zhujincheng` 上屏为 zhujincheng），与关闭输入法（Ctrl+Space）统一走 `flush_session`。**2026-08-21 改回：焦点切换
    不再打断会话**（用户设计原则：Esc/Enter/空格上屏或 Ctrl+Space 关闭前会话不因焦点切换断开；Alt+Tab 期间预编辑
    保留、返回继续，语义同小狼毫）。`flush_session` 仅保留给 Ctrl+Space（apply_openclose）与 Deactivate。
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
- [x] **按键直通白名单（2026-08-14）**：`config.json` 新字段 `passthrough_apps`（exe 名列表，
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
- [x] **四态表示统一（2026-08-21，35-review §H3 / 36-review §D5 结案）**：全仓只留 iuv-core
  `ImeState` 一个四态类型——IPC `Register/StateSync/CtlResult` 与 UI `ToolbarSpec` 直接持它；
  `ToolbarState`(u8×4)/`CTL_FIELD_*`/`to_toolbar()` 裸元组/`set_field(u8,u8)` 全删；线编码唯一
  转换点 = `From<ImeState> for [u8;4]`/`TryFrom`（runtime.rs，序 mode/width/script/punct，
  非法字节解码整条拒绝——顺带结掉 37 号"ToolbarState 解码值域校验"）；`CtlCmd::SetState{field,value}`
  改 `SetMode/SetWidth/SetScript/SetPunct(bool)` 四变体（ctl 通道 tag 0x01..0x04，无线字段序数协议，
  Register/StateSync/CtlResult 线上字节不变）；iuv-win 加 iuv-core 直接依赖（35 §5 已批准方向）。
  改动：iuv-core（runtime.rs 转换+3 测试）、iuv-win（Cargo/msg/codec/mod/lib 导出+测试重写）、
  iuv-ui（toolbar.rs ToolbarSpec.state + render.rs 夹具）、iuv-tsf（daemon_client 签名/daemon_host/
  mode 直传快照/text_service apply_ctl_cmd match 四变体）、iuv-daemon（toolbar mod 实例表/window
  点击类型化翻转）。契约/35/36/37 同步。测试：工作区全绿（303）。H2（english_mode 双源）按用户决策暂不做。
- [x] **语言栏右键菜单工具栏项文案动态化（2026-08-21）**：菜单项按 daemon 当前
  显隐偏好二选一（已显示→「隐藏工具栏」/已隐藏→「显示工具栏」/查询失败→中性
  「显示/隐藏工具栏」兜底）：新增 `Request::GetToolbarVisible`（0x0D）+
  `Response::ToolbarVisible{visible}`（0x03），选管道查询而非 shm 加字段（避免段布局偏移
  移动破坏热部署后旧 DLL 读段）；daemon `ToolbarHost::visible()` + main.rs 显式处理；
  tsf `DaemonClient::toolbar_visible() -> Option<bool>`；langbar 自绘菜单（show_menu 每次
  弹出前刷新第一项，MenuWindow 新增 set_items）与 InitMenu 官方路径同改。改动：iuv-win
  （msg/codec+测试）、iuv-daemon（toolbar mod/main）、iuv-tsf（daemon_client/langbar/
  menu_window）。测试：工作区全绿（305）。
- [x] **daemon 重启后工具栏自愈：无条件注册 + 显示判定放宽（2026-08-21）**：
  日志实测两轮盲区——①Activate 发生在 daemon 死亡期 → 只发 Active 被丢弃；②Register
  恰在 daemon 重启窗口期失败（新开记事本 Activate 即时触发但管道未就绪，静默丢失）
  → 所有自愈路径都汇聚在按键驱动的 poll，不打字不恢复。修复（纯事件驱动，**不用
  轮询定时器**——用户决策弃用 SetTimer 手法，对齐小狼毫零定时器架构）：
  **(a)** `register_instance` 删 `registered` 门（Cell 字段删），每次 Activate 无条件发
  Register（daemon `instances.insert` 幂等覆盖）→ 焦点切回任意 iuv 应用即自愈；
  **(b)** daemon 显示判定放宽（用户拍板语义「全局显隐变量决定，有输入焦点即显示」）：
  `poll_foreground` 不再要求前台窗口 pid:tid 精确命中 active 实例（时序脆弱）——
  `visible && 任一活动实例` 即显示；渲染态优先级 = 前台命中 > 最近激活实例
  （ToolbarInstance.seq 单调序号，Active{true} 分配）> 默认四态。
  **已知盲区（接受）**：daemon 异常重启 + 用户停在原窗口完全零交互 → 工具栏陈旧到
  下一次任意交互（打字即恢复）；正式使用不重启 daemon，升级/变更后注销规避。
  改动：iuv-tsf（text_service CtlApplier/daemon_host 无条件注册+daemon_poll_tick/
  key_routing 复用）、iuv-daemon（toolbar mod seq/window 判定放宽）、契约 32 同步。
  测试：工作区全绿。
- 点子库（暂不做，2026-08-19 记录）：**Tab 键用途**——整句翻译（在线 API：空闲 0.5s 触发、候选 N+1 槽、
  Tab 高亮 + 空格上屏）与自动补全（Tab 钉选当前候选续打，不结束会话），语义分配未定，`29-tab-ideas.md`
- **M9 可自定义贴图皮肤框架——调研定稿/挂起（2026-08-20，未实现）**：候选窗换肤 = 自研 `IUVSKIN01`
  （`skins/<name>/manifest.json` + 多区域 PNG，9-patch 缩放，部分贴图渐进增强，加载失败降级 light/dark），
  零新增依赖（tiny-skia 默认 `png-format` + `draw_pixmap` 缩放已确认）。**Lua 插件兼容已否决**（调研实测：
  librime 不内置 Lua、Weasel 默认不带、全 GitHub 用户级 Lua 插件仅 ~4 个合计 <100 星——`33-skin.md` §1）。
  皮肤格式互操作合法（红线：不抄搜狗/QQ 解析代码；只做自研格式）。**挂起原因**：前置 M8 悬浮工具栏
  （feat-toolbar 分支，效果差）需先改进。`33-skin.md`
- 后续：M3 整句增强(LMDG)/模糊音 · **M4 跨平台渲染候选窗——已实现（2026-08-16）**：
  tiny-skia+cosmic-text 绘图（crates/iuv-ui）+ D2D/DComp 呈现（ui/candwin.rs）+ 浅色/深色主题
  （2026-08-22 起扁平细边框，阴影已移除——见 f56e41a 条目），`19-m4-cross-render.md`
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
  - **M4~M6 验收清单备忘**（2026-08-16 完成）：M4 真透明圆角/深色主题/不抢焦点/多显示器 DPI
    （阴影项已失效：2026-08-22 移除改细边框，见 f56e41a；2026-08-17 已修 BeginDraw 关联 bug，
    候选窗此前不可见）；M5 语言栏右键菜单两项；M6 双进程即时一致/守护杀死降级/设置页热载
- [x] **设置-常用 = 新 TSF 实例初始状态（28-initial-state-settings.md，2026-08-19 落地）**：
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
- [x] **Excel 首字母直接上屏修复（2026-08-21 已结案：设计变更定稿）**：Excel 单元格首键输入，
  composition 落在**编辑栏** context，Excel 随即把 TSF 焦点切到**单元格编辑器**（同进程）——`OnSetFocus`
  曾把这种内部焦点移动误判为 Alt+Tab 级切换而 `flush_session`，首字母原文上屏（日志实测：`n`/`c`/`m`
  首键 `GetTextExt` 编辑栏宽矩形 + 紧跟 flush；直接进单元格窄矩形的 `i`/`e` 不丢）。**三版迭代**：①同线程
  判定（`GetBase/GetActiveView/GetWnd`+线程比较）——实测对 Excel 不可靠（单元格编辑器可能无窗口/异线程）；
  ②会话新生(<500ms)跳过 flush + 下一键重锚——首字母保住但**双份**（重锚在另一 context 建新 composition，
  旧编辑栏 composition 被 Excel 终止后残留首字母；加 cancel 旧 composition 又触发 Excel 过渡把新 composition
  也终止 → 会话降级）；③**定稿（只删不增）**：`OnSetFocus` **不再 flush**——焦点切换永不打断会话，仅隐藏
  候选窗防悬浮其他应用，session/composition 原样保留（用户设计原则，语义同小狼毫：Alt+Tab 期间预编辑保留、
  返回继续；Excel 首键后后续键对编辑栏 composition 继续 `set_text` 替换，无双份）。改动：iuv-tsf
  text_service.rs（OnSetFocus 减为两行）、mode.rs/key_routing.rs（删 `reanchor_on_focus_change`/
  `focus_on_same_thread`/`session_age_ms`/`reanchor_pending` 全部机制）。测试：工作区全绿（COM 胶水靠手测）。
- [x] **自绘候选窗抑制改名单驱动（2026-08-20 已修）**：微信打字 `ceshi` 到第 4 键候选栏消失
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
- [x] **全角行为（2026-08-19 落地）**：`initial_state.width == Full` 时**会话外直通路径**套
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
- [x] **工具栏悬停光标修复：类默认箭头 + 功能钮手指头（2026-08-22，手测通过，53f4c18）**：
  所有自绘窗口类注册 `WNDCLASSEXW` 走 `..Default::default()` → `hCursor = NULL`，
  DefWindowProc 对 NULL 类光标**不设光标**——悬停时残留上一进程的光标形状（实测忙等漏斗）。
  修复：iuv-win `popup.rs`（候选窗/菜单窗类注册）与 iuv-daemon `toolbar/mod.rs`（工具条+tooltip
  类注册）补 `hCursor = LoadCursorW(IDC_ARROW)` 默认值；`bar_wnd_proc` 新增 WM_SETCURSOR 臂——
  hit_test 命中功能钮（四态/齿轮）设 IDC_HAND、logo/空白设箭头并返回 1（WM_SETCURSOR 的
  lparam 不含坐标，GetCursorPos − GetWindowRect 原点换算客户区坐标；拖拽捕获期系统不发此
  消息，无需特判）。语言栏右键自绘菜单保持普通箭头（用户拍板：手指头仅浮动工具条用）。
  测试：工作区全绿（win32 胶水靠手测）。
- [x] **自绘窗口去阴影改细边框 + 根治工具栏命中区偏移（2026-08-22，手测通过，f56e41a）**：
  用户反馈工具栏悬停可点区相对图标**整体左上偏移**（图标右下沿点不到、左上外侧反而能点）
  ——根因：`render_toolbar` 返回矩形为内容坐标，而绘制经 `render_to_surface` 叠加了阴影偏移
  `sx = shadow_size×scale`（surface 四周留 2×shadow_px 阴影边），文档契约写「含阴影偏移」
  实现漏加 → 命中区比绘制内容偏左上 10~12px（125%/150% 缩放下）。借用户视觉改版需求
  （阴影过时）一并根治：`render_to_surface` 删阴影层与外围 margin——**surface 尺寸 =
  内容精确尺寸，内容坐标 = 表面坐标 = 客户区坐标**，daemon 三处 hit_test（hover/按下/
  WM_SETCURSOR）零改动自动与图标重合；边界改 `theme.border` 细边框，宽度
  `(scale).round().max(1)`（100%/125%→1px、150%+→2px，用户选定 round 规则），描边路径
  内缩宽度/2 使整条边完整落在位图内（外缘贴齐边缘不被裁半）。候选窗/语言栏菜单/tooltip/
  工具条四类窗口统一扁平化（共享渲染路径，外部消费者零改动）。改动全部在 crates/iuv-ui
  （theme.rs 删 shadow/shadow_size 字段、paint.rs 删 draw_shadow 死代码、render.rs/toolbar.rs
  闭包去 sx 参数）；测试适配（采样去手动 shadow 补偿、尺寸断言纯内容化）+ 新增边框像素断言
  `render_candidate_flat_border_no_shadow` + 工具栏几何测试补「按钮矩形完整落在 surface 内」
  回归锚。测试：工作区全绿（iuv-ui 43 含新用例）。
- [x] **Word 上屏后光标落在新文字前面：EndSession 补选区收尾（2026-08-22，手测通过，d72809e）**：
  「composition 结束后光标放哪」TSF 规范未定义、由应用自定：终端/notepad 自动把光标放到文本
  尾端所以不暴露；**Word 恢复自己记录的选区锚点（= composition 起点）**→ 光标回到新上屏文字
  前面。上网查证对齐两大开源实现的同款收尾（weasel `_InsertText` 与微软官方血统 Metasequoia
  `_AddCharAndFinalize`，注释原文 "insertion point just past the inserted text"）：`SetText` 后、
  `EndComposition` 前，显式 `range.Collapse(TF_ANCHOR_END)` + `context.SetSelection`；cancel 空串
  删除路径共用（折叠回原点，语义同样正确）；与预编辑路径 `SetTextSession` 既有收尾一致。
  改动仅 iuv-tsf composition.rs（EndSession 结构体 +context 字段）。测试：工作区全绿
  （COM 行为靠手测：Word 2007 光标确认在新词后面）。
- [x] **dev-deploy 构建三路并行 + daemon 独立 target 目录（2026-08-22，e061e65）**：
  串行三链每轮固定 ~2 分钟（脚本日志实测 119s/120s）：①x64 tsf release；②x86 tsf 独立 target
  全树重编一遍；③daemon `--features dev` 与 x64 同目录但特性集不同，共享依赖互踢缓存。改
  PowerShell Start-Job 三路并行（x64-tsf ∥ x86-tsf ∥ daemon，各车道独立捕获输出与退出码、
  失败逐车道打印详情后 throw），daemon 走 `CARGO_TARGET_DIR=target-daemon` 彻底解除构建锁与
  特性集耦合；产物路径 `$daemonSrc` 同步更新、`.gitignore` 补 `/target-daemon`。预期稳态
  120s → 40-70s（取最长车道）；daemon 车道首次全量编译一次性成本（已预热 3m10s）。
- [x] **设置窗重复点击改 Win32 还原/置前 + 根治关窗后幽灵重开（2026-08-22，手测通过，f022278）**：
  齿轮点击时设置窗已开 → 直接 FindWindowW + ShowWindow(SW_RESTORE) + SetForegroundWindow
  （学任务栏 SC_RESTORE 手法），不再积压 `open_settings` 标志——旧机制主线程阻塞在 eframe
  循环不轮询，标志残留到关窗后被消费 → 设置窗幽灵重开（日志实测：开着点齿轮无反应、关闭后
  窗口自己弹出）。**egui 每帧 logic() 方案已否决**：最小化窗口无 WM_PAINT → winit 不派发
  RedrawRequested → 无帧 → ViewportCommand 永远执行不到（实测五次点击零反应）；
  **SW_RESTORE 单独使用跨线程激活会被静默跳过**（还原后仍被前台窗口压住），必须补显式
  SetForegroundWindow。`settings_open` 标志新增：进 run_settings 前置位/返回后复位，管道线程
  据此分流「重开 vs 置前」。改动：iuv-daemon state/main/settings 三文件。测试：工作区全绿
  （手测：最小化一键还原置前 ✓、遮挡置前 ✓、关窗无幽灵重开 ✓、关窗后正常打开 ✓）。
- [x] **设置窗打开时居中于所在显示器工作区（2026-08-22，手测通过，edb6e30）**：
  egui 0.36 ViewportCommand 无居中命令、OuterPosition 是逻辑坐标还需换算 DPI——走 Win32
  直操（同聚焦套路）：creator 回调时机（原生窗口已建、首帧未画 → 零闪烁）FindWindowW +
  GetWindowRect（物理尺寸）+ MonitorFromWindow(NEAREST) 工作区 + SetWindowPos(NOSIZE|
  NOZORDER|NOACTIVATE)。基准 = 窗口实际落地的显示器（多屏跟随系统放置，不写死主屏），
  工作区而非整屏（下沿不被任务栏压住）；全程物理像素运算，PMv2 进程天然 DPI 正确。
  改动仅 iuv-daemon settings.rs（center_window_on_screen + creator 回调一行接线）。
- **中英切换已改系统机制（2026-08-12）**：`OPENCLOSE` compartment 真相源（系统"输入法/非输入法切换"热键驱动，
  OnChange 统一响应；语言栏点击归一写 compartment；Shift 切换已移除；**激活初值 = config `initial_state.mode`**，
  中文默认 = 激活即打开）。前置条件：用户在
  高级键设置把"输入法/非输入法切换"设为 Ctrl+Space（"切换输入语言"热键让位，Win+Space 仍可用）。
   已知遗留（已修 2026-08-14）：有活动候选时按热键关闭，未确认输入按原文上屏（原 bug：
   只清内存态不终止 composition → 带撇号分节预览残留；Alt+Tab 同根因一并修复）。
- [x] **候选窗跟随宿主布局变化（2026-08-23，手测通过）**：打字出候选后拖拽标题栏/滚轮/缩放，
  候选窗钉死旧屏幕坐标不跟随。根因：光标量取只发生在按键驱动的 SetTextSession edit session 内，
  无键事件即无人重查 GetTextExt。方案 = TSF 官方 **ITfTextLayoutSink** 事件驱动跟随（小狼毫同款
  机制、零定时器）：`OnSetFocus` 焦点文档就绪即挂 sink 到 top context（幂等：同 context 指针
  比对跳过；null focus 判空跳过）；`OnLayoutChange` 守卫（组词槽非空 + 同 context）→
  `Composition::query_caret` 只读会话（TF_ES_SYNC|READ，尾端锚点与打字路径一致；文档锁定/
  clipped/全零矩形一律 None 保持原位）→ `caret.set` + `ui.move_to` 平移（隐藏态自带 no-op，
  不复活窗口，符合「焦点切换不打断会话」）。**v2 改动面缩减**：sink 直挂 TextService 第 6 接口 +
  挂载点移 OnSetFocus（对比首版独立 LayoutSink COM 对象 5 文件 ~230 行 → 2 文件 +180 行）。
  **崩溃修复（同日，v2 首部署实测）**：`pdimfocus.unwrap()`——Ref::unwrap 对 null panic 且穿透
  extern "system" 回调 = 宿主进程 fail-fast abort（0xC0000409；WER 实锤故障模块 iuv_tsf.dll
  固定偏移，每开一个记事本数秒内连崩 7 次）；TSF OnSetFocus 会传 NULL document mgr（小狼毫
  `_InitTextEditSink` 开头 `if (pDocMgr == NULL) return TRUE;` 实证）。修复 = `as_ref()` 判空 +
  OnSetFocus 整体套 guard()（红线「iuv-tsf 绝不 panic 到宿主进程」，其余 sink 回调本都有 guard）。
  DPI 说明：候选窗 ULW 与 GetTextExt 同在宿主进程内同一坐标系，天然免疫小狼毫跨进程渲染的
  坐标缩放病（用户实测无轨迹放大感）。改动：iuv-tsf text_service.rs（字段/advise/unadvise/
  follow_layout/OnLayoutChange/OnSetFocus 判空）、composition.rs（query_caret + RepositionSession
  只读量取会话）。测试：工作区全绿（313）；手测 notepad 拖拽/滚轮/缩放平滑跟随、Alt+Tab 往返
   与 Excel 多 context 回归正常；事件日志部署后零崩溃，日志 `[follow]` 逐条跟随实锤。
- [ ] **隐藏工具栏后切应用复活（2026-08-25，代码修复，未验收不入库）**：
  用户在资源管理器语言栏菜单「隐藏工具栏」（`sh.visible=false` 已写盘 toolbar.json）→
  切到浏览器工具条又显示。根因：daemon `apply_event` 的 FocusGained 分支只看 OS 窗口运行时
  标志 `self.visible`、从不查全局偏好 `sh.visible` → 浏览器线程 `OnSetThreadFocus` 发信号即
  无条件 `show()`；违反 §32 原始规格「切回 iuv → 按偏好重新显示」。修复 = drop 锁前捕获
  `pref_visible` 作显示前置条件：偏好关闭仅 upsert 绑定实例不显示（日志「保持隐藏（偏好关闭，
  仅绑定）」）；重开走 ToggleVisible 既有「绑定活跃→立即恢复」分支闭环。全仓 `show()` 仅
  两处调用（FocusGained 已守卫 / ToggleVisible 重开天然偏好=true），无其他旁路。
  改动仅 iuv-daemon toolbar/window.rs（apply_event + 注释）。测试：iuv-daemon 全绿（10）；
  手测清单：资源管理器隐藏→切浏览器保持隐藏、切回仍隐藏、菜单重开立即恢复（位置/四态正确）、
  正常焦点跟随显隐回归、daemon 重启后偏好生效。
- [x] **39 引擎 Rime 化改造 Step1–3**（2026-08-26，分支 `feat/rime-engine`，任务书 `39-rime-pipeline.md`）：
  三步走落地——**Step1** 拆分引擎核心与适配层：新增 `api.rs` 顶层接口（`ImeEngine::translate`
  输入串→分段+候选 / `preedit` 高亮候选→预编辑串，`jian` 导航吉安显 `ji'an` 快赢兑现），
  routes.rs→classic.rs 承接全部生成逻辑（rank_plans 编排自 session 收编），Candidate 增
  `score` 字段；**Step2** librime 内核 Rust 改编：`src/rime/{syllabifier,translator,poet,mod}.rs`
  ——音节图三类拼写边（Normal/Abbreviation 双族/Completion）、逐起点桶收集三态键形查询
  （压缩简拼键 concat / 音节值 join' / 补全 prefix 展开）、poet arena 版 DP+Beam 组句、
  整句闸门（无全跨可靠词才组句）与分类词流（补全置顶→纯拼→简拼沉底）；librime 的
  Segmentation/Context 状态机不移植（打字期恒单段，段状态归会话层，裁决记录任务书 §13.1）；
  **Step3** 过渡开关 `Config.engine: classic|rime`（TSF load_engine 装配点 + REPL --engine，
  切换需重载生效）+ Backspace 改 rime 式**逐字退已选词**（多字词退末字、音节还原回未确认区；
  classic 同步启用）。RimeEngine 与 classic 共享 `Arc<Dict>`——M2 调权/自造词/隐藏跨核心同源。
  BSD-3 归属声明入派生文件头。测试：rime 行为 10 项 + engine_switch 3 项 + 既有回归迁移，
  workspace 329 绿；真词库对拍与 classic 全对齐。**二轮性能重构（同日，`3a7e50e`）**：
  首版逐路径/序列 DP 桶收集在真词库长句组合爆炸（chuangqianmingyueguang 整句缺失、最坏 143s）。
  管理员指示「学习小狼毫秒出」→ 通读 librime `table.cc/dictionary.cc/syllabifier.cc`，
  按 Table::Query 同构重写为**词典游标引导 BFS**：BFS 携带键串游标走图，每音节步
  `Dict::has_code/has_prefix` 零分配探针剪枝（exhausted 即砍枝，后者二分 upper_bound
  不受等长码簇影响），词条物化延迟到桶标记统一取；两族简拼键形由键串构造自然统一、
  特判全删；起点限定音节边界。性能档案见任务书 §14：长句全 translate 39ms、床前明月光
  置顶恢复、9 输入对拍全部语义对齐。**续接态字丢失修复（同日二次）**：degua 逐词上屏丢 xi、选老师后「换」字汤——根因为续接态
  raw 带撇号，origins/consumed_parts 按无撇号长度累加致坐标系错位（多起点桶全丢→句通道
  静默+中段词消失）。修法：F1 origins 改图推导（Normal 边终点∪0）；F2 边界表扫描 raw
  实际字节（跨撇号跳位）；F3 会话级回归钉死（选德国/老师后尾从 xi 续、喜欢可达、
  上屏=德国老师喜欢吃水果）。**部署实证（2026-08-26 20:16 dev-deploy）**：
  config.json `"engine":"rime"`，notepad 新进程日志三连证——引擎加载成功（125.5 万词条）/
  候选核心 rime / 加载完成 38ms 就绪，打字会话无异常。**遗留（任务书 §13.7 待复核）**：
  shigechengy 整句选词质量分歧（是个车那个月 vs 是个成员）；Raw kind 显式类型标记替代
  UI 魔法字符串（跨 iuv-ui/tsf 渲染契约，保守推迟）；箭头键位 rime 语义迁移（管理员拍板后置）；
  λ 打分校准与烘焙后删 classic。
- [x] **快捷键双槽可配 + 全局热键 + 设置页游戏式录入**（2026-08-28，分支 `feat/keymap-settings`，任务书 `41-keymap-settings.md`，已手测通过并入 main）：
  M7 键位热载收口。**数据模型**：`Keymap` 重写为 13 功能 × 主/备两槽（`Combo` 支持
  Ctrl/Alt/Shift/Win+基础键，序列化 `"Shift+Left"` 式）；`Key` 增 Tab/Delete/Home/End/Insert/F1-F12；
  旧 `Vec<Key>` 数组配置迁移 shim（TSF 与 daemon 双路径同规）。**会话内快捷键**：TSF `route_key`
  会话内组合键查表归一化（翻页/候选移动/调权/隐藏）；`map_key` 删导航/翻页键硬编码——物理键
  会话内语义完全由 keymap 决定（命中归一化、miss 放行给应用，清除即失效；候选移动默认补
  Up/Down 备槽保肌肉记忆）；keymap 热载经既有 config_epoch 每键 poll 天然生效（无需新开应用）。
  **全局热键**：daemon `RegisterHotKey` 注册中英/全角/简繁/标点/设置/工具栏六功能（Alt 随便绑，
  普通软件做法与 TSF 完全独立），WM_HOTKEY 复用工具栏 on_click 的 focused→CtlClient 分派；
  设置窗打开时 `FocusLost` 守卫（settings_open 保留 focused——设置窗是自家配置 UI 不算失焦，
  热键继续作用于打开设置窗前焦点所在应用）。**设置页游戏式录入**：点击录入框 → egui 事件流
  捕获组合键（`Event::Key`，官方注释明说给 input-capture UIs 用；弃用 WH_KEYBOARD_LL——winit
  消息泵宿主下回调从不触发，日志实锤）；Esc 取消、Backspace 清除、纯字母无修饰拒绝提示、
  会话红线（Alt/Ctrl/字母禁）、全局红线（≥1 修饰/Ctrl+Space 警告）、跨功能冲突检测；
  录入态经 `CaptureMode` 事件临时注销全部全局热键（RegisterHotKey 系统级抢键会拦截录入）。
  测试：workspace 全绿（约 347）；手测通过（简繁 Ctrl+Shift+F 翻转、清除键位即失效、
  录入回填/取消/清除、设置窗打开热键继续生效、录入态热键被吸收）。
- [x] **打字延迟收尾：关日志后剩余每键开销定点消除 + perf 埋点回归**（2026-08-29，分支 `perf/latency-polish`）：
  **起因**：反馈「候选比敲键慢一丁点」。在设置页关闭 `key/uielem/caret/candwin` 四个日志模块后
  卡顿消失，据此回读 `%TEMP%\iuv-tsf.log` 验证——**每键日志从约 25 条降到 3 条**，其中 `uielem`
  系占 17 条（TSF manager 每次 `UpdateUIElement` 回拉全量候选，每个 getter 都写日志；`GetString`
  6386 条 / 571 键 ≈ 11 次/键），这是「关闭即见效」的主因。
  **意外收获**：日志里躺着旧版本遗留的 `[perf]` 微秒埋点（当前代码已无，全仓 grep 为 0），
  成了唯一的实测依据（571 次按键）：`route` 3~6us、`onkey` 典型 100~1000us（尖峰 24~68ms）、
  `settext` 84~118us、**`render` 1.2~1.9ms 极稳定**。据此**推翻了先前按代码观感排的优先级**——
  真正吃时间的是渲染，而一度被重点怀疑的 Config 深克隆与 `TF_ES_SYNC` edit session 实测都很小。
  **剩余开销**（注销重采的新日志 518 行）：`[follow]` 403 条（78%）、`do_edit_session: GetTextExt
  失败` 80 条（**无 `[tag]` 前缀，而 `log.rs` 对无 tag 消息恒放行 → 配置关不掉**）、`[commit]` 23 条；
  渲染仍约 2ms（用「GetTextExt 失败 → 首条 follow」的时间戳差值测得，稳定 2~3ms）。
  **四项改动**：① `log.rs` 恢复 `perf_tick`/`perf_record_with` 埋点（route/onkey/settext/render/
  dispatch 五处，detail 走惰性闭包——关闭时连格式化都不做，只多一次原子读）；开关是**独立的
  `Config::perf_probe`（默认 false）**——一度挂在 `disabled_log_modules` 下，但后者是 denylist
  语义（未列出即记录），会让埋点在新配置下默认打开，等于每键多 5 次文件写入，恰好抵消关闭
  日志换来的手感（2026-08-29 浏览器实测变卡）。② `composition.rs` 的 `trace_step` 失败日志补 `[edit]` 前缀使其可
  配置关闭，并把分步描述全改为静态——原先调用点预先 `format!`（含预编辑文本前 32 字符的
  `take(32).collect()`），每键为一段看不到的日志白做字符串分配。③ 光标量取 `GetTextExt` 在
  Electron/Chromium 宿主上实测 **100% 失败**（`0x80040206`），每次按键白跑一次跨进程调用并写一条
  关不掉的日志；新增 `caret_probe_fails` 连续失败计数（沿用现有 `Rc<Cell<>>` 模式，随会话新建
  自然失效），**连续失败 3 次后判定宿主不支持并停止尝试**（取 3 而非 1：个别应用文档未就绪时会
  短暂失败后恢复，首次失败即永久禁用会让它再也拿不到候选窗位置）；打字路径与布局跟随
  `query_caret` 共享同一计数、后者整体早退。④ 布局跟过去重：`follow_layout` 量取到的坐标与当前
  一致即返回（实测每键两次内容相同的布局事件），`move_to` 目标位置与当前窗口位置一致则跳过
  `SetWindowPos`。**预期每键减少**：3 次跨进程光标调用、3 条日志写入、2 次 `GetWindowRect`+`SetWindowPos`。
  **notepad 验证（2026-08-29 二轮）**：新开记事本打字，日志佐证三项生效——① 全局 `[edit]` 计数 **0**，
  即记事本里 `GetTextExt` 从未失败、`caret_probe_fails` 恒为 0，**「不支持标记」没有误伤**，
  follow 坐标随打字正常右移（1606→1624→…→1714）；② 布局跟过去重生效，`follow/onkey` 从旧值 2.0
  降到 1.32（降不到 1.0 是正确的——记事本光标每键右移约 13px，坐标真变了就必须移动）；
  ③ `render` 1.3~1.7ms，与 Electron 旧数据的 1.2~1.9ms 一致，**确认渲染是输入法自身固定成本、
  与宿主无关**。**同一轮发现两个问题**：一是 `route` 埋点区间包错——`self.dispatch()` 在 match
  分支内被圈了进来，导致该列实为「整键总耗时」（30904us ≈ onkey+settext+render+dispatch 之和），
  已改为只包 `route_key`；二是 `onkey` 尖峰 17~62ms（典型 1.6~2.3ms，尖峰占比约四成），已排除
  两个假设：**不是日志 IO**（onkey 内部无任何日志调用）、**不是算法复杂度**（与输入长度无相关性：
  `len=22` 时 53ms 而更长的 `len=24` 只要 2.3ms）；「首次缺页后页面常驻」也被排除——同一个 `y`
  隔两秒再打，两次都在 60ms 上下。剩余最可能是**工作集颠簸**（词库 mmap 页被换出）。  为此新增
  `iuv-core/src/perf.rs`：引擎内部计时只做「转发」不碰 IO（跨平台纯 Rust 约束），输出由平台层
  注入的 sink 决定（TSF 侧转到 `[perf]` 日志）。细分点按引擎分别布置：
  `classic.rs::translate` 三段 `onkey.segment`/`onkey.rank`/`onkey.generate`；
  **本机 config.json 是 `"engine":"rime"`，实测走的是 rime 核心而非 classic**，故该侧另加四段
  ——`onkey.seg`（切分重排）/ `onkey.graph`（音节图构建）/ `onkey.buckets`（**唯一真正访问词库
  mmap 的一步，尖峰若落在这里即坐实缺页**）/ `onkey.assemble`（候选组装 + Poet 整句 DP）。  **另注**：浏览器会话日志里 `perf=0`、失败日志仍是带 `ec=` 参数的旧格式，说明
  新开窗口复用了部署前就存在的 Edge 进程、加载的仍是旧 DLL，**「失败 3 次后停用」在 Electron
  宿主上尚未实测**（需彻底退出浏览器进程再测）。
  **浏览器验证（2026-08-29 三轮，彻底退出 Edge 后新开）**：`edit/按键 = 0.11`（旧行为每键 1~3 条），
  规律正是设计意图——**每个会话前 3 键尝试、失败后全部跳过**，本轮会话平均约 27 键故摊薄到 0.11；
  `route` 修正生效（30904us 假数据 → **11~19us**）。onkey 细分：`graph` 0.2ms 稳定、
  `assemble` 1.3~1.6ms 稳定、`seg` 9~24ms、`buckets` 10~50ms（唯一访问词库 mmap 的一步）。
  **尖峰定性**：296 组样本按长度排开，**同一长度下差异达 50 倍**（`len=5` 时 1903us vs 73057us，
  而 `len=24` 五个样本全在 1.4~3.7ms），**彻底排除算法复杂度**；基线随长度增长正常
  （len 1~6 → 0.1~3ms，len 60~86 → 8~15ms），尖峰是叠加的 +30~70ms 且分散在多个阶段
  （四段之和比 onkey 总量还少 11~27ms）→ 确认为**工作集颠簸**（词库 mmap 页被换出），与二轮
  推测一致。**最终精简（2026-08-29 收尾）**：经三轮实测评估，撤销两处收益测不出来的微优化——
  `text_service.rs::follow_layout` 坐标去重与 `candwin.rs::move_to` 位置去重（记事本 follow/onkey
  仅 2.0→1.32，坐标每键在变去不掉多少；且 caret-probe-disable 已让 Electron 宿主上 query_caret
  整体早退、follow 日志随之消失，去重的边际收益被覆盖；两处都引入额外提前返回分支，
  收益/风险比不划算）。**保留三项**：① perf 埋点机制（含 `perf_probe` 独立开关与引擎细分）——
  本次全部结论都建立在埋点数据上，而第一版靠的是日志里**旧版本遗留**的埋点数据，纯属运气；
  埋点默认关闭、零开销，其价值是未来排查能力而非当前性能。② caret-probe-disable（浏览器场景
  砍掉约 90% 无效跨进程调用与日志，记事本零误伤）。③ `trace_step` 补 `[edit]` 标签 + 描述
  静态化（修复「无 tag 消息恒放行」的漏洞，顺带删掉为日志服务的 `take(32).collect()`）。
  **最重要的一条认知（供后续参考，避免重复排查）**：**卡顿主因是日志 IO 本身——在设置页关闭
  `key/uielem/caret/candwin` 四个模块即拿到绝大部分收益（每键日志约 25 条 → 3 条），
  这不需要任何代码改动；代码层面的改动是叠加在其上的小头，且集中在 Electron 类宿主。**
  排查此类问题的正确顺序是：先用量化的埋点数据定位，**切勿按代码观感猜**——本次初版猜测基本
  全错（把 render 排到第 4、把 Config 深克隆与 `TF_ES_SYNC` 当重点，实测都只有几十微秒，
  还把 dispatch 圈进 route 造出一列假数据）。**遗留**：`render` 记事本 1.3~1.7ms / Edge
  3.1~3.4ms 是唯一剩余的每键固定成本，也是唯一还能靠改代码削掉的；onkey 尖峰只在超长会话
  （连打几十个字母不上屏）下出现，日常打字碰不到，要治需做词库页面预热，收益/风险比不划算，
  暂不做。测试：workspace 全绿（355）。

