# 工作状态台账

> 本文件 = 完整工作台账：每项落地的根因/方案/改动/测试记录。新条目追加到文末。
> AGENTS.md 只保留导航与活跃事项速览并指向本文件；细节权威来源 = git log 与
> `docs/plan/` 任务书。结案条目不删除（历史档案）。
> **提交约定：允许提交 = 已测试通过**——台账不设也不回溯维护「待手测」状态。
> 注：34~37 号任务书/评审文档已删除（对应结论已并入台账正文）；`docs/research/` 目录已删。

## 活跃事项速览

### 进行中：M10 薄客户端重构（49 号，分支 `feat/m10-thin-client`，未并 main）

**当前状态（2026-09-27，接手前必读）**：P0-P3 全部落地并真机回归通过，共 10 commits；
main 未动，`use_engine_server` 开关（默认 false）保证 main 行为随时可回。

- **已交付**：P0（`iuv-win::ipc::rtt` 实测：长连接 P99=13µs vs 一请求一连接 ≈2ms）→
  P1（`crates/iuv-proto` 线上契约：帧格式/三枚举/serde codec）→ P2（`iuv-win::transport`
  长连接：握手/认证/推送/截止时间）→ P3a（`platforms/windows/iuv-server` 引擎服务进程）→
  P3b（TSF 薄客户端 A/B 接入）。真机已验证：本地/远端双模式打字、杀 server 透明降级。
- **真机坑已修**（细节见文末 M10 各条）：`let _ = server` 语句末析构毁管道；
  提权启动 = 高完整性管道中完整性应用连不上（改受限计划任务启动 + windows_subsystem
  去黑窗）；400+ 候选三份载荷顶破截止（裁每键 UiElement 推送）。
- **定档**：单键截止 300ms = 挂死保命线（非延迟策略）；引擎单键实测 17-58ms
  （125 万词库，`iuv-server.log [perf]` 观测线 ≥10ms 持续收集）。
- **测试**：`scripts\m10-build.ps1` → `m10-deploy.ps1`（-SkipBuild/-NoServer）→
  `m10-uninstall.ps1`。日志 `%TEMP%\iuv-tsf.log` / `iuv-server.log` / `iuv-script.log`。
- **P4 首切片已落地（2026-09-27）**：配置热载改服务端持有（iuv-server 后台监视
  config.json → 引擎热载 + 请求捎带 `Push::ConfigChanged`）+ 远端模式按键路径零轮询
  （`poll_client`/SHM 读取从按键路径删除，`daemon_poll_tick` 远端分支只剩进程内原子量
  比较的主题收敛）。细节见台账。
- **P4 剩余**：服务端自渲染候选窗（届时 KeyOutcome 的 candidates/all_candidates/reading
  过渡字段与每键全量载荷随之裁撤）；ctl 反向通道与 toolbar signal 收敛至 transport
  （依赖 daemon→server 演进，工具栏/设置页迁入服务端后整体消失）；用户库 SHM 写者
  移交服务端（修工具栏权重显示滞后）。
- **远端模式已知盲区（P4 首切片引入，接受）**：daemon 重启自愈退回 Activate 重发——
  原按键路径轮询承担的「打字即恢复」不再有；正式使用不重启 daemon，daemon→server
  合并后问题消失。
- **下一步 P5**：失效语义 C 落地（TSF 检测断连 → 拉起 iuv-server → ResumeToken
  重绑；协议字段已留位：`Hello.resume` / `Push::SessionAttached`，服务端尚未实现重绑）。
- **过渡期已知限制**（P4 剩余项，非 bug）：远端模式下调权/造词不经旧 daemon（工具栏
  SHM 权重显示可能滞后）；用户库版本注入跳过（服务端持有用户库）；flush 原文 =
  composition 去撇号（用户手打引号边角）。~~服务端配置热载未接~~（P4 首切片已消除，
  改配置即时生效）。
- **环境注意**：本机测试进程做文件 IO 报 os error 5（存量环境问题，疑杀软，干净树
  复现，与本仓库代码无关）——相关存量测试在本机红属正常。

### 未开工 / 挂起

- M3 整句增强(LMDG)/模糊音
- 符号/emoji 候选、学习候选（微软对齐已知差距，见 M1.5 条目）
- M9 可自定义贴图皮肤框架（调研定稿/挂起；前置 M8 工具栏已多轮打磨，可重新评估）
- 点子库：Tab 键用途（`29-tab-ideas.md`，暂不做）
- 设置页高级页缺外层 ScrollArea：第三个卡片（全屏行为）被挤出 640×480 固定窗口可视区且无
  法滚动。**第二次踩此坑**——修法见 `keymap_tab` 2026-08-28 注释（包
  `ScrollArea::vertical().max_height(ui.available_height() - 12.0)`）。本次提交未修，留待后续；
  期间该开关恒为默认开启（全屏隐藏功能本身可用，仅入口不可达）。

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
  （每字母 top-500 词频序）。依据：微软实测清单（原 `docs/research/msime-probe-checklist.txt`，该目录已删）
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


## 2026-08-29 · 全仓精简（死代码/重复实现/文档漂移，分支 refactor/code-cleanup）

- [x] **根因**：多智能体轮番迭代后仓库出现四类债：①确认死代码（`Candidate.score` 全程计算无消费者、`Span.tags`、`Dict::effective_weight`、`OpenccTable::empty`、`UnigramLm._entry_count`、`MappedFile::len`、`Key`/`Effect`/`PageInfo`/`SessionEnd`/`Candidate` 的 serde 派生——IPC 走手工 codec、serde 只服务 Config，iuv-data 的 `serde` 依赖与 iuv-daemon 的 `windows-core` 依赖随之删除）；②重复实现（api.rs `strip` 与 lib.rs `strip_apostrophes`、rime/mod.rs 三个并行测试模块、iuv-tsf/ui/mod.rs 与 iuv-ui/snapshot.rs 逐字重复测试、tsf/daemon 两份近乎复制的文件日志器、两份 `strip_jsonc_comments`+keymap 迁移、三份用户数据目录解析、signal.rs 内联复制 pipe imp 原语、install/dev-deploy 两份 `Test-ArchRegistered`、langbar 菜单项 id/文案三处硬编码）；③文档漂移（README 仍宣传已否决的 Tauri 与已删除的 GDI；30→02 改号约 15 处断链；status.md 速览与台账矛盾；指向已删 34~37 号文档与 docs/research/ 的断链）；④scripts 文档缺失。
- [x] **方案**：结构去重四件套——路径解析统一到新 `iuv_core::paths`（LOCALAPPDATA→APPDATA\Local→USERPROFILE\AppData\Local→HOME；TSF 侧兜底由幻路径 `C:\Users\Default\...` 改为 `%TEMP%\iuv`，两者在 env 缺失时都找不到真实词库，语义等价但不再误导）；文件日志器收敛到 `iuv_win::logger`（denylist `[tag]` 解析唯一实现，两进程行为由代码保证一致；iuv-win 原 fn 指针钩子机制删除，`log_line` 内惰性 init）；daemon 的 JSONC/BOM/keymap 迁移改调 iuv-core 公开实现（新增键位字段不再要改 3 处）；langbar 菜单收敛为常量表 + `handle_menu_id` 单一分发。零风险项：死代码删除、`tests2/tests3` 并入 `tests`、编译期音节助手（SYLLABLES/is_syllable/greedy_segment）自 dict.rs 移入 format.rs 并顺带对齐 lue→lve/nue→nve 归一（与 Quanpin 一致）、Sentence.weight 字段删除（原供已删的 score）。**classic 管线未动**（39 号计划书另行收尾）。
- [x] **改动**：详见分支 diff；文档侧重写 README、修全部断链、更新 39/41 号任务书状态行、AGENTS.md scripts 清单补 download-opencc/clear-data/compare-engines/convert-main-icon、00-overview §5 索引标注归档去向。
- [x] **测试**：workspace 全绿（351 通过/1 忽略；净减 4 个重复/死测试）；`cargo check` 零警告；iuv-tsf release 构建通过。**待手测**：dev-deploy 部署后打字链路、设置页（日志模块开关/恢复默认）、语言栏右键菜单、工具栏显隐——尤其 TSF 与 daemon 用户库路径合并后的首启。

## 2026-08-29 · 39 号 λ 打分校准（分支 feat/rime-lambda-calib，未并 main）

- [x] **根因**：39 号收尾项「λ 打分校准」起步即发现打分机制未落地——syllabifier 的 credibility 是死数据、候选无统一 score、组句平局先到先得、λ 无参数出口；且 §13#7 shigechengy 分歧（是各成员国 vs classic 是个成员）的真因不在打分参数，而在 `Dict::prefix` 截断序 bug（无用户库时按码序返回、64 截断把高频「成员」排挤出 cheng'y 补全桶——生产有用户库时 merged 排序反而掩盖了此差异，REPL/对拍与生产隐性分叉）。
- [x] **方案**：机制先建、参数后调——① BucketEntry.cred 贯穿 BFS/合并/poet 词格（librime dictionary.cc:164 credibility 语义）；② poet λ 参数化 + 平局决胜重建（poet.cc:88-109：权重降序→少词优先→词长序列字典序）；③ `Candidate.score` 重落地（词=log 权+cred、句=路径权重；仅诊断展示不参与排序——整句保底置顶与类别序 §13#4 结构不动；上轮精简曾删无消费者的 score，本轮有真实消费方且契约 §8.1 已载）；④ `Config.rime_lambda/rime_spelling_penalty` 参数出口（默认=librime 原值）；⑤ prefix 范围物化后**先权重降序再截断**（兑现契约注释，classic candidate_prefix 同受益）。
- [x] **改动**：iuv-core（poet/translator/syllabifier/mod/candidate/config）、iuv-data（dict.rs prefix）、iuv-repl（batch 输出增 score 列）、compare-engines.ps1 语料 9→12 条、契约 §8.1/§8.2、39 号任务书 §13#7 结案+#11 新裁决+§15A 校准基线/结果附录。
- [x] **测试**：新增 dict.rs prefix 权重序回归测试（含用户库覆盖场景）+ rime real_dict_poet_graph_dump 真词库诊断（ignored，dump 图/桶/组句 + λ 扫描）；workspace 全绿。真词库验证：W1 默认参数 12 条语料与基线逐字节一致（零行为变化）；prefix 修复后与 classic 首屏 **12/12 全对齐**，shigechengy=是个成员且 λ∈[-20,0] 恒胜。**遗留（已兑现 2026-08-29）**：烘焙后删 classic（等管理员确认，见下条）；dev-deploy 部署手测留给管理员。

## 2026-08-29 · 39 号收尾：烘焙后删 classic（分支 feat/rime-single-core，管理员确认，手测待部署）

- [x] **根因**：39 号 §2/§3/§6 预定的过渡终点——rime 已烘焙稳定（真词库部署打字无异常、λ 校准后 12 条语料与 classic 首屏全对齐），管理员确认删除 classic 引擎与过渡开关，维护面回归单一。详见任务书附录 §16。
- [x] **方案**：删 classic 四件套——① 删 `classic.rs`（430 行核心 + `impl ImeEngine for Engine`）与 `EngineChoice`/`Config.engine` 开关，`Engine::new` 内部装配 `RimeEngine`（`ime: Arc<dyn ImeEngine>`），`start_session*` 恒产 rime 会话；② 删过渡脚手架（`attach_core`/`alt_core`/`shared_dict`/`Session::new`/`with_runtime`/REPL `--engine`/`compare-engines.ps1`），`rank_plans` 迁入 `api.rs`；③ 旧 `"engine"` 配置键加 `migrate_engine` shim 清理；④ 契约 §8/任务书/台账/AGENTS/README 同步。
- [x] **补齐 rime 功能缺口（删 classic 暴露）**：üe 输入形归一（图构建前 lue→lve/nue→nve，替换长度不变坐标系安全）；`candidate_prefix` 前缀联想（RimeEngine 字段 + 词条流后追加，classic 同款）；简拼键形拼接修复（单字母简拼边直拼 concat——旧实现恒 join `'`，`jj` 拼成 `j'j` 命中不了压缩式简拼键，§13#2 裁决兑现）；**大写保形**（图构建不再 `to_lowercase`——大写字符不可达分隔，`Hello` 兜底原文而非被 `he` 音节截胡，`niHAO` 仍从 ni 前缀出词）。
- [x] **测试**：会话层 18 项 classic 语义断言改写为 rime 语义（词优先/简拼展开/类别序，对照表见任务书 §16.4）；`engine_switch.rs` 删 classic 默认行为测试、`api_seam.rs` 改经 `RimeEngine` 测契约。workspace 全绿（约 354）+ cargo check 零警告；真词库 REPL 12 条语料与 §15A 基线全对齐（shigechengy=是个成员、chuangqianmingyueguang=床前明月光、nhmsx=你还没 命中 concat 修复）。**待手测**：dev-deploy 部署后打字链路（新 DLL 已装配 rime 唯一核心，无配置开关）。

## 2026-09-01 · 全屏自动隐藏工具栏与桌宠（对齐 QQ 输入法）

- [x] **根因**：全屏看视频/打游戏时工具栏与桌宠恒悬顶层不隐藏（QQ 输入法会隐藏）。
  `docs/pet/ARCHITECTURE.md` §7 只写了「全屏游戏检测：沿用候选窗隐藏策略」的意向，全仓
  **无任何全屏检测代码**；而显隐治理（40-toolbar-show-hide-governance.md）是纯信号模型，
  判定式里根本没有全屏维度。
- [x] **方案**：新增第三个**正交慢速维度**「全屏抑制」——显隐收敛为
  `should_show = 偏好 visible && 焦点绑定活跃 && !全屏`。判定 = 前台窗口矩形覆盖所在显示器
  整屏（容差 8px；比 `rcMonitor` 而非 `rcWork`，全屏窗口会盖住任务栏）。
- [x] **为什么是 1 秒轮询而非事件驱动**：浏览器 F11 全屏、最大化转全屏时**前台窗口句柄本身
  没变**，只有矩形变了 → `EVENT_SYSTEM_FOREGROUND` 不触发；`EVENT_OBJECT_LOCATIONCHANGE`
  则在每次拖动窗口时高频触发。低频轮询是覆盖全部场景且开销最小的方式（每次 3 次系统调用、
  无分配，微秒级）。**必须排除桌面**（`GetShellWindow()` + 类名 `Progman`/`WorkerW`）——
  桌面自身矩形恒等于整屏，不排除会导致「回到桌面就永远隐藏」。探测失败返回 `None` →
  保持上次状态不动（不猜、不 panic）。
- [x] **与 40 号裁决的边界（关键）**：40 号定稿禁止前台查询参与**焦点显隐判定**（失败记录 #1：
  TSF 焦点通知常跑在系统更新前台窗口之前，一次性查询导致连续切换连丢）。本次轮询结果
  **只驱动全屏抑制这一个开关**，绝不参与焦点归属判定；焦点仍 100% 由 TSF 信号驱动。
  两者正交：全屏抑制容忍 1 秒延迟，不与焦点信号竞争真相源。
- [x] **内聚性取舍（砍掉的绕行）**：轮询定时器建在工具条线程，`WM_TIMER` 就在该线程消息循环
  内 → 结果本就在正确线程，**不再绕 FIFO 入队**（原计划的 `BarEvent::Fullscreen` 变体与
  `Shared.fullscreen` 字段一并砍掉），`mod.rs` 因此只加 1 行 `mod fullscreen;`。
  判定集中于 `should_show()` 一处，各事件分支只维护状态、末了统一收敛。显式**不重构**
  `apply_event` 控制流（内部 PetModel 重置时机、`sync_pet_timer` 调用点都是反复调过的），
  回归面压到最小。
- [x] **桌宠零额外代码**：宠物与工具栏同窗渲染，`hide()` 已含 `kill_pet_timer()` → 随工具栏
  隐藏并自动停帧，直接兑现 ARCHITECTURE §7「全屏游戏检测，宠物动画同步暂停」的性能目标
  （常驻 CPU 增量 <1%）。
- [x] **改动**：`toolbar/fullscreen.rs`（新，探测模块 + 7 项纯函数单测）、`toolbar/window.rs`
  （`fullscreen` 字段 + `should_show()` 三维判定 + `FS_TIMER_ID` 定时器 + `WM_TIMER` 分派 +
  `Drop` 配对 KillTimer；`FocusGained`/`ToggleVisible` 分支改走 `should_show()`）、
  `toolbar/mod.rs`（1 行 `mod` 声明 + 窗口就绪后 `start_fs_timer`）、`config.rs`
  （`hide_on_fullscreen` 字段 + 读写 + 往返测试）、`settings.rs`（高级页勾选框 + 保存广播）。
  **未改 iuv-core**：Windows daemon 专属行为，不进跨平台契约，故 01-contract.md 无需同步。
- [x] **测试**：workspace 全绿（476 通过 / 2 忽略，含新增 7 项 `covers_monitor` 纯函数单测 +
  1 项配置往返）；`cargo check` 零警告。单测覆盖：精确覆盖 / 容差内 / 超容差 /
  **最大化不误判**（高度因任务栏少 40px 远超容差 8，关键回归项）/ 普通窗口 / 副屏原点 /
  退化矩形。
- [x] **手测（2026-09-01，管理员实测通过）**：浏览器视频全屏、MCPBE（Minecraft 基岩版）、
  多显示器环境、退出全屏后自动恢复、Alt+Tab 离开全屏——五项全部符合预期。
- [x] **未覆盖场景（已知边界，非待办）**：D3D 独占全屏游戏未实测。判定逻辑与已通过的 MCPBE
  同源（前台窗口矩形覆盖 `rcMonitor`），风险低，后续遇到该场景再补测。另：设置页
  「全屏行为」开关入口暂不可达（高级页缺外层 ScrollArea），见顶部活跃事项——功能本身
  恒为默认开启，不影响上述实测结论。

- [x] **48 号 · 简拼键音节碰撞修复（dictc 编译期过滤）**（2026-09-19，任务书
  `docs/plan/48-abbrev-syllable-collision.md`，同日入库 6b34917）：敲 `fa` 1 号出「方案」
  压过「发」——根因 = M1.5 预生成简拼键（`fang'an`→`fa`）无"非完整音节"守卫，撞
  完整单音节 exact 查询档（§4.2"完整单音节无歧义→纯单字"），满权重零罚分进池；
  判别实验 `la` 桶混入恋爱/立案/两岸等 l+a 形态词。修法：`compile.rs::abbrev_of`
  对串联结果过 `format::is_syllable`（iuv-data 现成标准音节表），命中即不生成简拼键
  ——`fa`/`la` 形同微软/小狼毫只出单字，`nh`/`xa`/`tam` 等非音节形简拼照常。
  改动：`compile.rs`（过滤 + 注释）、`compile_format.rs`（+1 碰撞回归测试）、
  `01-contract.md` §3 简拼键注释（隔离前提升级为生成不变量）。重编译
  `data/iuv.imedic`（旧库备份 `iuv.imedic.bak-20260919`，gitignore 产物不入库）。
- [x] **测试**：workspace 全绿（18 套件）+ clippy 零警告；repl 实测 `fa`→发/法纯单字、
  `la` 无恋爱、`n'h` 简拼出词、`fang'an` 方案 1 号、`x'a` 新旧库逐条一致（无误伤）。
  分数列微移为词库 total_weight 分母变化，符合预期。**真机手测通过**
  （2026-09-27 管理员实测；dev-deploy 前装机器已装的旧 imedic 需换新：脚本按
  install.ps1 词库链自动重编或手动拷贝）。

- [x] **49 号 · M10 架构重构立项：薄客户端 + 引擎服务端，IPC 协议定稿**（2026-09-27，
  任务书 `docs/plan/49-thin-client-arch.md`，分支 `feat/m10-thin-client`）：
  「每应用进程一份引擎」→「全系统一个 iuv-server.exe + 薄 TSF 客户端」。现存 4 套 IPC
  （用户库管道/SHM/ctl 反向通道/toolbar signal）收敛为一条长连接三平面
  （热路径 REQ/RESP · 控制面 · 状态面 latest-wins PUSH）。§6 五项拍板：
  ①失效语义 **C+A**（断连拉起 + ResumeToken 重绑重生；失败窗口透明降级；客户端兜底引擎否决）；
  ②超时**放行按键** + 基线失效全量重同步（Key.full 位）；
  ③**Effect 瘦身**（热路径只回 KeyOutcome，候选仅 CAP_UIELEMENT 走 Push::UiElement）；
  ④认证密钥文件放用户配置目录、仅当前用户可读、不轮换；
  ⑤codec **全量 serde**（放弃「iuv-proto 零依赖」的绝对化约束）。
  实施顺序：P0 前置（perf_probe 新增 ipc_rtt 实测热路径往返，没有数据不开工）→
  P1（iuv-proto crate）→ P2（transport + 握手/认证）→ P3（热路径打通）→
  P4（四套旧 IPC 收敛、按键路径零轮询）→ P5（失效语义落地）。

- [x] **49 号 P0+P1：`iuv-proto` crate 落地 + `ipc_rtt` 实测基建**（2026-09-27，分支
  `feat/m10-thin-client`，协议定稿后按 §5 分期实施）：
  - **P1**：新 crate `crates/iuv-proto`（线上契约唯一权威，49 §4）——8B 帧头
    （payload_len u32 LE / kind / flags bit0=URGENT / stream_id u16 LE），kind 5 值
    **双向分编号**（客户端REQ/服务端REQ/客户端RESP/服务端RESP/PUSH，帧自描述）；
    C2S/S2C/Push 三方向枚举 + 线上载荷类型（Key/ImeState/瘦身版 Effect/Candidate 剔 score/
    UserMutation/CtlCmd 镜像现有语义）；全量 serde+postcard（拍板 §6.5）；stream_id
    偶奇分配器（回绕跳过在途，耗尽返回 None）。**恰好一帧**校验：截断/残留/保留 flags 位/
    未知 kind/未知变体一律拒整帧（49 §4.8 纪律）。帧预算有测试锁死：Key 请求 ≤24B、
    None 增量应答 ≤24B。同步：01-contract §2.1 快照校准 + §2.2 加 iuv-proto 行；
    49 §4.2 kind 表改 5 值。
  - **P0**：`iuv-win::ipc::rtt` 基准模块（同套管道原语测 per-request-connect vs
    persistent 双形态 echo，预热 20 轮不计样本，NotFound 短重试过 accept 间隙）+
    tsf `send_request` 挂 `ipc_rtt` 埋点（perf_probe 开关内，`request_kind` 明细分六类）。
    开发机首批实测：persistent P50=8µs/P99=13µs，per-request P50=17µs/P99≈2.0ms
    （accept 间隙主导），connect=1.5ms——**长连接 150×@P99**，印证 49 §4.0 推翻旧否决。
    真机（打字机）数据待采集回填（`cargo test -p iuv-win --test rtt_bench -- --nocapture`）。
  - **测试**：iuv-proto 18 项契约测试全绿（往返/拒整帧/帧预算/回绕/协商）；rtt_bench 绿；
    clippy 全 workspace 零警告；`cargo check --workspace` 零警告。
  - **存量环境问题（与本批无关，干净树复现）**：本机全新编译的测试进程做文件 IO
    （%TEMP% 写/SHM 创建/配置读写）一律 os error 5「拒绝访问」，shell 直写同路径正常
    ——疑似杀软/策略拦截未签名测试二进制，致 iuv-core config / iuv-data / SHM 等
    **存量** IO 类测试在本机红。非沙箱同样复现；49 号相关测试不受影响。

- [x] **49 号 P2：`iuv-win::transport` 长连接传输落地**（2026-09-27，同分支）：管道
  `iuv.service.v1`（BYTE 模式，显式分帧），**std 线程 + overlapped IO**（实现层修订：
  不引 async 运行时，避免 TSF DLL 依赖膨胀；overlapped 提供型别化截止时间，读写可限时
  可取消——修订理由已写进 49 §4.1）。服务端：accept 线程（可取消）+ 每连接一线程 +
  连接上限（§4.7）；握手三关 = 首帧 Hello → 版本协商（无交集回 `Err(VersionMismatch)`
  断连）→ 密钥校验（不符回 `Err(Unauthenticated)`）；会话建立即推 `SessionAttached`
  令牌（重绑语义 P5）。客户端：读线程分发 RESP/PUSH + 写互斥；`request(req, urgent,
  deadline)` 同步有截止，**超时烧号不复用**（迟到应答绝不串号）；最后一个 client drop
  关连接（读线程自然退出）。`Hello` 补 `caps` 能力位（服务端取交集回 `HelloAck.caps`）；
  proto 新增 `decode_payload` 公开 API（transport 分帧后解载荷）。附 `auth_file`
  load_or_create（BCryptGenRandom 生成 32B token，create_new 并发安全，ACL 随用户
  profile 继承；显式 DACL 后置安装器）。
  **测试**：`tests/transport.rs` 7 项集成全绿——互连+SessionAttached 推送、Ping/Pong +
  热路径 Key + UserMutation、版本不匹配类型化报错、认证拒绝、超时 Deadline 后恢复（迟到
  应答不串号）、4 线程并发多路复用、连接上限拒绝；clippy 零警告；iuv-proto 18 项仍绿。
  存量 SHM 环境失败不变（见上条）。

- [x] **49 号 P3a：`iuv-server.exe` 引擎服务落地（无头可测）**（2026-09-27，同分支）：
  新 crate `platforms/windows/iuv-server`（lib+bin）。main 装配与 tsf engine_host 同源
  （词库/配置/用户库/简繁表 → `Engine`；词库失败退出非零——服务端无透明模式意义）；
  共享密钥 `load_or_create_token(iuv_dir)`；`--pipe` 可覆盖管道名。lib = `EngineService`
  （transport `ConnHandler`）：**每连接一个 `EngineSession`**（对齐旧架构每实例一会话），
  `Key` → `Session::on_key` → `Effect` 映射瘦身 `KeyOutcome`：
  - composition 增量（基线相同回 `None`；`full=true` 强制全量——§4.5.2 重同步服务端侧）；
  - `commit` 字段升级为 `end: Option<SessionEnd>`（Commit(text)/Cancel 语义精确，
    proto 破坏性变更，未发布故版本仍 v1）；补 `reading` 过渡字段（P3b 客户端自绘候选窗需要）；
  - `Caps::UIELEMENT`：`KeyOutcome` 带当前页候选 + 每键 `Push::UiElement` 全量候选
    （过渡期客户端自绘；服务端自渲染落地后移除）；
  - `C2S::ImeState` 新增（客户端 OPENCLOSE 真相源 → 服务端会话运行时四态）；
  - 过渡边界：`UserMutation` 无独立入口（调权/造词/屏蔽只经按键在引擎内生效）；
    配置热载待接（改动需重启服务端，P4 收敛）；`FocusChanged` 不断会话（38 号）。
  **测试**：`tests/hot_path.rs` 6 项无头全绿——合成 `nihao`+Space 提交「你好」、
  composition 增量（Left 页首夹紧语义核实）/Right 移动选中、`full=true` 强制全量、
  `EndSession` 后新会话、Esc 取消、无 UIELEMENT 能力零候选推送。clippy 零警告。
  服务端侧热路径契约全部可自动化验证；**P3b（TSF 薄客户端化 + Test/KeyDown 去重）待做，
  完成后需真机打字回归**。

- [x] **49 号 P3b：TSF 薄客户端化（A/B 开关，待真机回归）**（2026-09-27，同分支）：
  新模块 `com/remote_host.rs`——进程级 `RemoteHandle` 连 iuv-server：
  - **Test/KeyDown 单槽去重（§4.5.1）**：`key_test` 发请求缓存裁定（同键重复 Test 复用）；
    `key_down` 命中缓存零 IPC、未命中现场处理；
  - **截止时间（§4.5.2）**：每键 20ms；超时 → 放行 + degraded → 下一键 `full=true`
    全量重同步（测试验证 full 标志真实翻转）；`Busy` 不触发；
  - **失效语义 A（§4.5.4）**：断线/Closed → offline，按键全部放行（P5 补方案 C）；
  - 路由判定**全部留在客户端**（keymap/passthrough/全角/中文标点依赖本地态）；
    `KeyOutcome` 经 `merge_outcome` 以 `last_effect` 为基线组装 Effect，复用既有
    dispatch/候选窗/composition 渲染路径（本地/远端共用一套 UI 代码）；
  - 客户端配置副本（`Config::use_engine_server` 新字段，默认 false=现状零行为变化）：
    远端模式不加载词库/引擎；daemon 配置纪元热载走新 `DaemonClient::poll_client`
    （无引擎变体：用户库注入跳过，服务端持有）；
  - 四态同步：`after_runtime_change`/会话开始时 `C2S::ImeState`（差量，未变化不发）；
  - flush_session：远端原文 = composition 去撇号（过渡近似，服务端补 pending_text 后消除）。
  **测试**：remote_host 3 项单测全绿（去重/超时→degraded→full 重同步真实翻转/
  断线放行）；TSF 39 通过 + 2 存量 SHM 环境失败；clippy 零警告；workspace 编译通过。
  **真机回归（管理员）**：dev-dep 后 ①默认 `use_engine_server=false` 回归现状；
  ②config.json 加 `"use_engine_server": true` + 启动 `iuv-server.exe` → 打字验证：
  中文拼音/候选窗/空格上屏/Esc 取消/Shift 中英/Ctrl+Space/点简繁/翻页/游戏内候选。

- [x] **49 号 P3 真机回归通过（远端模式打字全链路）**（2026-09-27，记事本/多应用）：
  远端模式连接 0ms、远端会话内提交、候选窗/uielem 数据流全部正常；失效语义 A 验证
  通过（服务端不可达 → 全放行，应用零卡死）。真机暴露并修复三坑：
  ① `let _ = server` 语句结束即析构 → 服务端管道消失（进程活着客户端全放行）；
  ② 提权脚本直启 server → 高完整性管道，中完整性应用连不上 error 5 →
     改受限计划任务（用户上下文）启动 + windows_subsystem 去黑窗；
  ③ 单字母 400+ 候选三份全量载荷间歇顶破截止 → 裁每键 UiElement 推送（单份走
     KeyOutcome.all_candidates）。
  **定档数据（iuv-server.log `[perf]`）**：引擎单键 17-58ms（125 万词库 rime 生成），
  据此单键截止定档 300ms（保命线语义，非延迟策略——放行漏字 + 基线分叉比等待更伤）。
  过渡期遗留：`use_engine_server` 开关 + 客户端自绘候选（P4 服务端自渲染后收敛）；
  服务端慢键 `[perf]` 观测线 >=10ms 持续收集。

- [x] **49 号 P4 首切片：配置热载改服务端持有 + 远端模式按键路径零轮询**（2026-09-27，
  同分支）：P3 过渡期两个已知限制一并消除——「远端模式改配置需重启 server」与
  「按键路径读 SHM 检测配置纪元」。
  - **根因**：daemon 设置页保存 config.json 后只 bump SHM `config_epoch`（原子量），
    iuv-server 无人通知（引擎配置启动时一次性加载）；TSF 侧消费该纪元的唯一触发点
    在按键路径 `route_key → daemon_poll_tick → poll_client`（每键读 SHM 两个原子量，
    epoch 变化才 `Config::load`）。服务端主动推送通道在 transport 层不存在
    （conn 线程阻塞读循环，外部线程无法插写），但 `Reply::push` 已支持捎带。
  - **方案（传输层零改动）**：① iuv-server 新增 `config_watch` 后台线程——500ms
    stat config.json（mtime+len 对，原子 rename 保存下两者同变），变化 →
    `engine.set_config` + 日志禁用集热载 + 配置纪元（`AtomicU32`）自增；
    ② `EngineSession` 每请求处理时比对纪元（进程内原子读，非轮询），变化则在
    `Reply` 捎带 `Push::ConfigChanged{epoch, client_view}`——latest-wins 语义天然
    成立（客户端按 epoch 判新旧），连接建立时点即基线（不推旧值，客户端连接时
    自行 `Config::load`）；③ TSF 推送泵（原样丢弃推送）接 `ConfigChanged` →
    `Config::load()` 刷新进程级配置副本 + 纪元自增（`RemoteHandle::set_config`）；
    ④ 实例侧主题收敛：`daemon_poll_tick` 远端分支改 `apply_remote_theme_tick`——
    比对 `RemoteHandle.config_epoch()` 与实例缓存 `remote_theme_epoch`（两个进程内
    原子量，无 SHM/IPC/文件读），落后才 `ui.set_theme`。传播时序与旧路径相同
    （改动 → 下一键生效），磁盘读移到推送泵后台线程。
  - **P4b 按键路径零轮询**：远端模式 `poll_client` 删除（SHM 读取随之消失，本地
    模式 `poll` 原样保留——A/B 开关保证 main 行为不变）。daemon 上线翻转重注册
    随按键路径轮询一并移除：**已知盲区（接受）**= 远端模式下 daemon 重启后工具栏
    自愈退回 Activate 重发（原「打字即恢复」不再有；daemon→server 合并后消失）。
  - **顺手修复**：`iuv-win/tests/transport.rs` 存量编译错误——P3 修漏键给
    `KeyOutcome` 加 `all_candidates` 字段（a1fa127）时测试初始化器漏改，该测试
    文件在 HEAD 编译不过（与本次改动无关）。
  - **改动**：iuv-server（lib.rs 纪元字段+捎带推送、config_watch.rs 新增、main.rs
    装配、hot_path.rs +1 测试）、iuv-tsf（remote_host.rs 推送泵/纪元/apply_push、
    daemon_host.rs 远端分支重写、text_service.rs remote_theme_epoch 字段、
    daemon_client.rs 删 poll_client、key_routing.rs 注释）、iuv-win
    （tests/transport.rs 存量编译修复）。
  - **测试**：iuv-server 7/7（新增 config_epoch_change_pushes_config_changed_once：
    无变更零推送/纪元变化下一请求捎带/同纪元只推一次/client_view 取引擎当前
    配置视图）；iuv-tsf 40 通过 + 2 存量 SHM 环境红；iuv-win transport 7/7（修复后
    可编译）；workspace 其余失败全部为已记录存量 os error 5（36 处，统一
    PermissionDenied/SHM 0x80070005/PoisonError 派生）；clippy 全 workspace 零警告。
  - **待真机回归（管理员）**：dev-dep 后远端模式（`use_engine_server=true` +
    iuv-server）：设置页改主题/翻页数/键位 → 不重启 server，下一键生效（引擎 +
    候选窗主题）；杀 daemon → 工具栏不再打字恢复（预期行为，切窗口恢复）。
  - **P4 剩余（后续切片）**：服务端自渲染候选窗（KeyOutcome 候选字段裁撤）、
    ctl/toolbar signal 收敛（依赖 daemon→server 演进）、用户库 SHM 写者移交服务端。

- [x] **49 号 P4 真机回归:P4 首切片验证通过 + 修「间歇漏键」真凶(C2S::ImeState 无应答)**
  （2026-09-27,同分支）:
  - **P4 首切片真机验证**:新 server 日志出现 `[config] 配置热载监视`;远端模式连接
    0-3ms、打字正常;daemon 重启窗口期信号管道报错后自愈。配置热载生效链路(改配置 →
    `[config] 配置热载生效` → 下一键 Push::ConfigChanged)待管理员改一次配置验证。
  - **真凶实锤(日志时间线)**:远端会话首键候选窗晚 300ms + 偶发「远端请求超时 →
    degraded」,而服务端 on_key <10ms。notepad 会话逐行对齐:`.333 [key] 按键:j` →
    `sync_state` 发 `C2S::ImeState`(连接后 last_state=None 必发)→ `.334~.634` TSF
    线程阻塞在 recv_timeout(300ms)——**服务端 ImeState 臂只更新 runtime、无
    reply.respond,客户端等一个永远不会来的应答** → 必然超时 + 误标 degraded →
    `.635` 才继续走缓存命中的 key_down。今日 16 次超时全部同源;P3「间歇漏键」
    当时只修了载荷问题,此为残余真凶——首会话必中,每次中英/全半角/简繁/标点
    切换同样命中(各 300ms 卡顿)。
  - **修复**:`EngineSession::on_c2s` 对 ImeState 与全部 fire-and-forget 变体
    (FocusChanged/CaretMoved/SetMaintenance/CtlResult/UserMutation)回 `S2C::Ok`——
    协议纪律「每个 C2S 请求必须有应答」,杜绝「等不来的应答」整类问题。
  - **测试**:hot_path +`every_c2s_request_gets_a_reply`(ImeState/FocusChanged/
    CaretMoved 应答契约回归钉),iuv-server 8/8 全绿;clippy 零警告。
  - **待管理员**:重新 dev-dep(此修在 server 侧,需重启 iuv-server)后,远端模式
    首键应即时出候选;切换中英/简繁等不再卡 300ms;`[backend] 远端请求超时` 应归零
    (除非服务端真挂死)。

- [x] **49 号 P4 真机回归(二轮)：ImeState 应答修复验证通过 + daemon 完整性继承坑**
  （2026-09-27）:
  - **修复验证**：新 server(pid 8848)后全量日志**零**「远端请求超时」；首个远端会话
    首键 `[key] 按键:c → BeginUIElement 候选窗` 间隔 **1ms**（修复前 300ms）；
    连打 ceshi 全程无 degraded。
  - **新坑（M7 惰性拉起的完整性继承，待收敛）**：提权部署窗口的 conhost（高完整性）
    在 16:54 惰性拉起 daemon → 其信号/数据管道拒绝所有中完整性应用（error 5，
    notepad/Explorer/Edge/ZCode 全中招，工具栏断连）——与 P3 server 提权启动同款
    （server 已改受限计划任务，daemon 还是 CreateProcessW 惰性拉起）。**现场处置**：
    UAC 提权 taskkill 杀掉，下次任意普通应用 Activate 惰性重启即恢复中完整性。
    **后续方向**：daemon→server 演进（P4 剩余）后问题消失；短期若复发，可考虑给
    daemon 也套受限计划任务启动。
- [x] **49 号 P4 配置热载服务端侧真机验证通过**（2026-09-27 三轮）：设置页切主题
  Dark→Light，server 8848 日志 epoch 1→4（每次保存约 1.5s 内两次写，设置页双写、
  幂等无害）`[config] 配置热载生效` → 引擎 set_config，**无需重启 server**。
  Push→客户端接收链路本轮未被真机触发（改主题期间无远端客户端在线打字——
  在打字的 ZCode 是本地模式；push 搭下一请求便车无车可搭，且随后 server 重启
  epoch 清零、客户端重连时自行 Config::load 拿到新配置）。链路有无头测试覆盖；
  真机验证法：notepad 保持打字状态改主题，下一键应即切主题 + tsf 日志出现
  「配置推送 epoch=」。daemon 已以中完整性重启（ZCode 上线，error 5 归零）。

- [x] **49 号 P5:失效语义 C+A 落地——断连拉起 server + ResumeToken 重绑重放**
  （2026-09-27，分支 `feat/m10-thin-client`，待真机回归）:
  - **服务端重绑**（§4.4）: transport `on_connect` 增 `resume`/`token` 参数；
    iuv-server `EngineService` 持重绑注册表（token → 断连现场）——**断连时
    `EngineSession::drop` 把仍活动的引擎会话（core Session + composition 基线 +
    四态 runtime）按令牌存入**；EndSession/commit 已清空会话 → 无现场 = 令牌自然
    作废（§4.4 语义）。带 `Hello.resume` 重连 → 回绑现场 → 客户端 degraded 置位
    的下一键 `full=true` 强制全量应答 = **composition 重放**（复用 §4.5.2 机制，
    无需专门的回放报文）。TTL 5 分钟，`on_connect` 取用时顺带清扫（零定时器）。
  - **客户端重生**（§4.5.4 方案 C+A）: remote_host 推送泵捕获
    `SessionAttached` 令牌（每次连接更新）；请求失败（Closed/IO）→ offline
    透明放行（A）+ `schedule_revive` 后台重生线程（`reviving` 防重入）——
    首次尝试即拉起 **TSF DLL 同目录 `iuv-server.exe`**（CreateProcessW 继承宿主
    中完整性，P3 提权教训；在线时撞管道名静默退出无害）→ 带 `Hello.resume`
    重连 → degraded → 下键全量重同步；6 次未果（约 2s/次重连上界 + 250ms 间隔）
    保持透明，**Activate 兜底重试**（text_service Activate 挂 `schedule_revive`）。
  - **transport 真 bug 修复（重连压测 1/5 帧错乱实锤）**: 旧连接 client drop 后
    句柄值可被新连接 `CreateFileW` 复用，旧读线程的 `ReadFile` 会命中复用值、
    与新读线程瓜分字节流 → 帧错乱（`Malformed` 载荷截断）。**收尾协议重构**——
    ① 句柄改由读线程关闭（退出后无人再读旧值，复用无害）；② 读线程改 500ms
    tick 有界超时读（每轮查 closed，Drop 最坏一个 tick 收尾）；③ Drop 只置
    closed + 尽力 `CancelIoEx` + 有界 join；④ `wait_io` 超时路径修复「IO 恰在
    超时瞬间完成 → 字节数被丢弃」的丢数据窗口（有界超时下必踩）。
  - **测试**: iuv-server 10/10（+重绑回放：断连打 "ni" 重连后 full 首键回放
    "ni…"；+EndSession 作废：重绑得全新会话）；iuv-tsf 42 通过（+令牌捕获/
    重连恢复）+ 2 存量 SHM 环境红；transport 7/7；**重连压测 12 轮零失败**
    （修复前 1/5 帧错乱）；clippy 全 workspace 零警告。
  - **待真机回归（管理员）**: dev-dep 后远端模式 ①打字中杀 iuv-server → 按键
    短暂放行后自动恢复（iuv-server.log/任务管理器可见新进程），**未提交的
    composition 应保留**（重绑回放）；②服务端进程不存在时杀掉 → 打字自动拉起；
    ③ `Hello.resume` 无现场（超期/作废）→ 全新会话不报错。
- [x] **49 号 P5 真机回归通过：杀 server → 自动拉起 + 令牌重绑**（2026-09-27 18:22
  管理员实测）：notepad 打字中经任务管理器杀 iuv-server——时间线全对：
  `.206` 客户端请求失败「连接已断开」→ 透明放行 + 后台重生（失效语义 C）；
  `.209` 判定服务端不可达 → **客户端自行拉起 iuv-server.exe**（新进程 32ms 就绪、
  带 `[config]` 监视线程）；`.213` 首次重连未就绪（管道 error 2，预期）；
  `.464` **重生成功（令牌重绑）**——检测到拉起仅 258ms；`.596` 下一键直接在重绑
  连接上继续（会话内 Space），随后 ceshi 正常打字。全程零超时、零卡死。
  注：server **进程死亡**时保存的会话现场随进程消亡 → 重绑得全新会话（当前
  composition 丢弃、下一键重开），这是 C+A 设计内行为（现场回放只覆盖连接断开
  但服务端存活的场景）；无形态变化。

- [x] **49 号 P4 服务端自渲染候选窗落地（待真机回归）**（2026-09-27，同分支）:
  - **服务端**（新 `candwin` 模块）: 每连接一个 UI 线程 + ULW 窗口（命令驱动
    Show/Update/MoveTo/Hide/SetTheme，latest-wins），渲染/定位/圆角命中与 TSF
    客户端版同源（iuv-ui 软件光栅 → UpdateLayeredWindow）；**DPI 按 caret 所在
    显示器 GetDpiForMonitor 自算**（main 置 PMv2）；无点击选词（UI 线程触达不了
    连接线程的会话，待服务端主动 REQ 通道接线，键盘数字选词不受影响），悬停
    高亮保留。会话接线: Effect → effect_to_snapshot → 窗口命令；结束/空快照 →
    Hide；ConfigChanged 推送同时热载服务端窗主题。
  - **光标上报**: `C2S::CaretMoved` 双侧落地——客户端**只在锚点变化时**上报
    （打字期锚点恒定 → 绝大多数键零上报）；会话首键由 `query_insertion_caret`
    （selection 起点量取，composition 尚不存在）先行上报 → 服务端首帧即定位
    正确；dispatch 与 follow_layout 两路变化均上报（宿主拖拽/滚动时窗口跟随）。
  - **载荷裁撤（49 §4.5.3 兑现）**: 抑制判定 = `candidate_owner_apps` ∩ 客户端
    宿主进程名（每键读引擎配置 → 热载即时生效）。命中的连接（如 WoW）: 服务端
    窗静默 + KeyOutcome 携带候选数据源（游戏桥 UI 元素所需）；普通应用: 零候选
    载荷（显式空数组清客户端旧值，防增量合并留旧候选在游戏桥），**每键从几 KB
    降到几十字节**；caps=0 客户端仍 None（契约不变）。
  - **客户端**: 远端模式本地候选窗不画（`apply_effect` 增 `render_locally`，
    跳过快照/显隐/跳变判定——本地窗从未 show 即从未创建，零开销）；桌宠 typing
    信号撤销「候选非空」条件（服务端渲染后普通应用候选为空属常态）。
  - **测试**: iuv-server 11/11（+抑制命中带候选数据源；普通应用断言改零候选；
    无头测试不触发窗口——caret 未上报时 sync_candwin 早退）；iuv-tsf 42 通过
    (+2 存量 SHM 红)；clippy 全 workspace 零警告。
  - **待真机回归（管理员）**: dev-dep 后远端模式 ①普通应用打字：候选窗由服务端
    画（观察 iuv-server 进程窗口/日志），首键定位正确、跟随打字/拖拽/滚动；
    ②主题热载：设置页切主题 → 服务端窗即时切换；③WoW（wow.exe 在抑制名单）：
    游戏内候选栏照旧（客户端桥），服务端窗不出现；④鼠标悬停高亮正常、点击候选
    无效（已知过渡限制）。
  - **真机回归结果（2026-09-27 18:30 管理员实测）**：远端模式候选窗由服务端渲染
    **全部正常**；唯一问题 = **悬停候选窗指针变漏斗**——根因：candwin UI 线程阻塞在
    `rx.recv()`，无 Win32 消息泵 → WM_SETCURSOR（SendMessage）得不到响应 → 系统显
    忙等光标；hover 高亮/圆角穿透（WM_MOUSEMOVE/NCHITTEST）同样实际未生效。**管理员
    拍板暂不修**——待点击选词/服务端主动 REQ 落地时必须先补消息泵（GetMessage 循环
    或事件唤醒 + 泵集成），届时一并根治。
- [x] **49 号 P4 服务端候选窗真机回归通过 + ②用户库收敛第一刀**（2026-09-27）:
  - **服务端候选窗真机验证通过**（管理员实测，远端模式候选窗由 iuv-server 绘制
    正常）。唯一问题 = **悬停候选窗指针变漏斗**——根因：candwin UI 线程阻塞在
    `rx.recv()`，无 Win32 消息泵 → WM_SETCURSOR（SendMessage）无响应 → 系统忙等
    光标；hover 高亮/圆角穿透（WM_MOUSEMOVE/NCHITTEST）同样未实际生效。**拍板
    暂不修**——待点击选词/服务端主动 REQ 落地时必须先补消息泵（GetMessage 集成
    或事件唤醒），届时一并根治（悬停/穿透同批复活）。
  - **②(daemon→server 演进)首切片**：远端模式下 iuv-server 是用户库文件真相源
    （引擎调权/造词/隐藏走引擎本地写盘），daemon 内存态只是启动快照 → **设置页
    用户词库面板打开时按 `use_engine_server` 从文件重载**（缺失/损坏保留快照）。
    修掉已知限制「远端模式权重显示滞后」。文件写入无冲突（远端模式 daemon 管道
    写路径无人触发）。**已知过渡边界**：混合模式（部分应用本地引擎）下 daemon
    管道写会用陈旧内存态覆盖文件——待 `C2S::UserMutation` 接线（客户端统一经
    transport 写 server）后整体消除。
  - **M10 剩余路线图**：② 剩余——用户库混合模式收敛（C2S::UserMutation 接线）→
    daemon→server 全量迁移（工具栏/设置页/全局热键）→ ctl/signal 收敛（服务端
    主动 REQ + 客户端 reader 处理）＋candwin 消息泵补齐（根治悬停漏斗 + 复活
    hover/穿透 + 接通点击选词）；③ 收口（删 A/B 开关、core/proto 镜像归一、
    并 main）。
- [x] **49 号 服务端候选窗 + ②首切片 真机回归通过**（2026-09-27 22:36 管理员实测，
  21:38-22:36 日志窗口）: 零超时零异常；候选窗/主题热载（21:39 epoch=1 theme=Dark，
  服务端窗 SetTheme 同批生效）/锚点上报全链路正常。**P5 重生链路再验两轮**——
  21:42 WoW.exe 重生成功（令牌重绑，无现场→全新会话，抑制名单应用远端模式正常）；
  22:21 notepad 杀 server → 客户端 2.5s 自愈（拉起 + 重绑），server 6996 由客户端
  拉起后持续打字至 22:36 无异常。21:38 出现首例 `[resume] 断连保存现场`（真实
  会话带 composition 断连，保存路径工作）。悬停漏斗维持已记录状态（消息泵缺失，
  待点击选词批次根治）。

## 2026-09-27 · 49 号 ② daemon→server 全量迁移完成（七子提交，待真机回归）

- [x] **M10 ② 收敛全量落地**（同分支，子提交 `7f912e2`/`c05540e`/`ac2937c`/`19af3d2`
  /`8543b8e`/`fc4ed5c`(前条)/`0ea9935`）:
  - **proto**: 迁移变体补齐——`C2S::{TypingActivity, OpenSettings, ToggleToolbar,
    ToolbarVisibleQuery}` + `S2C::ToolbarVisible{visible}`。
  - **transport 控制面（49 §4.1 服务端主动 REQ 打通）**: 服务端每连接
    `ConnShared`（写互斥 + 在途应答表 + 写者计数）+ `ConnSender`（clone；
    `request(S2C::Ctl)` 同步等 `C2S::CtlResult`，3s 截止、超时烧号）；conn 帧
    循环增 `ClientResp` 路由 + 收尾协议（closed → 等写者归零 → 清在途——防句柄
    值复用错写连接，与客户端读线程收尾同类）。客户端 `ClientConfig.on_server_req`
    处理器（独立线程执行，阻塞 3s 不阻塞读线程；应答帧带原 stream_id）。
    测试 +`server_initiated_request_roundtrip`，transport 8/8。
  - **用户库**: `Engine::apply_user_mutation`（外部变更应用 + 本地写盘，语义与
    daemon 数据面管道同源）+ iuv-server `C2S::UserMutation` 臂 → 引擎应用 +
    SHM 发布（EngineService 持唯一 ShmWriter；混合模式本地实例经共享段一致）。
    测试 +`user_mutation_applies_to_engine`，hot_path 12/12。
  - **daemon 模块整体迁入** `iuv-server/src/daemon/`（toolbar/prefs/tooltip/
    fullscreen/window/settings/state/config/hotkey/capture/pet_assets/
    toolbar_icons/log，~5400 行，`crate::` 路径重写零逻辑改动）。出向依赖抽象:
    `CtlDispatch` trait（四态翻转分派）——window.rs 齿轮/热键 OpenSettings/
    ToggleToolbar 改进程内直调；server 实现 `TransportCtlDispatcher`（pid/tid →
    ConnSender）。`EngineSession` C2S 路由: FocusChanged/ImeState/TypingActivity →
    ToolbarSignal（pid/tid=握手报备）；OpenSettings → 主循环标志；
    ToggleToolbar/ToolbarVisibleQuery → toolbar 宿主。main: park 循环 → daemon
    同款主循环（OpenSettings → 主线程 eframe 设置窗 + hotkeys_changed + 兜底
    flush）；`DaemonState` 以 shm=None 构造（SHM 零双写者）。
  - **TSF 侧改线**（远端模式）: 焦点/四态/打字信号 → `C2S::{FocusChanged,
    ImeState, TypingActivity}`（`notify_*` 模式感知出口；四态信号远端 no-op——
    sync_state 已覆盖）；langbar 显隐查询/设置/工具栏开关 → transport；
    `C2S::Ctl` 经进程级提交钩子（ctl.rs `set_submit_hook`/`submit_cmd`，最近
    激活实例端点 PostMessage 应用，与旧 accept 线程同模式）。本地模式全保留。
  - **candwin 消息泵补齐（根治悬停漏斗）**: UI 线程 `WakeEvent`（CreateEvent）
    + sender `SetEvent` 唤醒 + `MsgWaitForMultipleObjectsEx(QS_ALLINPUT)` +
    PeekMessage 泵——WM_SETCURSOR 等 SendMessage 得到响应（漏斗根除），hover
    高亮/圆角点击穿透复活；事件句柄 Arc 计数，销毁竞态回环收敛。
  - **daemon 退役**: 删 `platforms/windows/iuv-daemon`（workspace members/AGENTS/
    README/契约同步）；dev-deploy 三路并行 → 两路（x64∥x86 TSF），守护进程部署
    节改为停历史残留进程；m10-build 四车道 → 三车道。
  - **测试**: workspace 全绿（仅存量 os error 5 环境红；iuv-server --lib 的
    7 失败 = 迁入的 daemon config/state 文件 IO 测试，同源存量）；clippy 全
    workspace 零警告。
  - **待真机回归（管理员，dev-dep + m10-deploy 后）**: ①远端打字 + 工具栏看板
    （焦点跟随/四态/桌宠——现在由 server 驱动）；②工具栏/全局热键四态翻转
    （transport Ctl 往返）；③语言栏菜单（设置页打开/工具栏开关/菜单文案）；
    ④候选窗悬停：指针应正常（漏斗根除）、hover 高亮生效；⑤混合模式调权 →
    server SHM 发布。**注意旧 daemon 需手动停**（deploy 脚本已处理）。
- [x] **49 号 ② 迁移真机回归（第一轮，2026-09-28 05:33 部署）**:
  - **核心链路全通**（05:33-05:40 日志窗口，server pid 19680 = 新架构进程）：
    ① **工具栏看板由 server 驱动**——`C2S::FocusChanged` → ToolbarSignal →
    激活/失焦/显示/隐藏全链路正常（ZCode/msedgewebview2 多实例焦点切换逐条
    对应）；② 远端打字正常（290 慢键 15-51ms，无一超 300ms）；③ 配置热载
    epoch 1-3；④ **零超时零 panic 零 candwin 错误**。
  - **遗留观察**：① 工具栏按钮/热键 Ctl 翻转本轮未触发（无 `[toolbar] 翻转`
    日志），transport 往返待实测；② 候选窗悬停指针/hover 高亮需肉眼确认（日志
    无从观测）；③ **daemon 进程复活**：部署脚本停掉后 1s 内被旧 DLL 本地模式
    进程（Explorer/taskmgr 等）惰性拉起（053313）——对远端客户端无害（互不相
    通），但混合期存在双工具栏可能；随 ③ 删本地模式或全部进程换新 DLL 后消失，
    过渡期可手动杀（无进程再自动拉起即稳定）。
- [x] **daemon 复活根治 + 残留清除（2026-09-30）**:
  - **根因（真机日志实证，非台账此前猜测的「旧 DLL 进程拉起」）**：TSF Activate
    无条件调 `ensure_daemon()`（text_service.rs，M7 惰性拉起未按模式分流）——远端
    模式薄客户端每次激活输入法都拉 `iuv-daemon.exe`（ZCode/taskmgr/Qoder 等逐条
    「已拉起守护进程」日志），杀掉即被下一激活进程拉回；安装目录残留 9-27 旧 exe
    使 CreateProcess 恒成功（deploy 只停进程不删文件）。
  - **修复**：① `ensure_daemon` 包进 `!use_server()` 分支（本地基线保留 M7 自启，
    远端 Activate 不再拉任何东西）；② dev-deploy 「停进程」升级为「停 + 删残留
    iuv-daemon.exe」（幂等），修正「无进程再拉起它」错误注释；③ 已删安装目录
    残留 exe。双保险：未重启的旧 DLL 进程再拉只会静默失败（文件已删）。
  - **验证**：进程表仅 iuv-server；删后 45s 观察零拉起日志；安装目录仅剩
    iuv-server.exe。
- [x] **server 登录自启 + 首连失败重试闭环（2026-09-30，重启真机验证通过）**:
  - **真机暴露双缺口**（重启后日志）：① server 无开机自启——deploy 的
    Iuv-ServerStart 是一次性任务（注册→启动→立即注销），此前靠 M7 惰性拉起兜底，
    关掉后重启即裸奔；② 首连失败永久放弃——`start_remote_load` 失败路径
    `REMOTE.set(None)` 把 OnceLock 槽占死（守卫 `get().is_some()` 恒真 + 后续
    `set(Some)` 静默失败），Explorer/notepad/ZCode 等全部「连接失败→远端模式
    透明」且手动起 server 也无法挽回，须重启宿主进程（P5 重生只覆盖「连上过
    再断开」，首连失败无重试无拉起）。
  - **修复**：① m10-deploy 计划任务改常驻（AtLogOn + RunLevel Limited +
    ExecutionTimeLimit 清零），m10-uninstall 对应注销；② `start_remote_load`
    失败改「拉起 iuv-server.exe 再试一轮（connect_server 自带 2s 重试窗）」，
    仍失败保透明、下次 Activate 重试（不再 set(None)）；加 `REMOTE_CONNECTING`
    原子防多实例 Activate 线程风暴。
  - **重启验证**：登录 server 即在位（PID 10848）；新进程全 0-3ms 首连成功
    （conhost/msedgewebview2/Explorer/taskmgr/ZCode/WorkBuddy）；关机瞬间旧会话
    三进程断连走 P5 重生全部成功；ZCode 实测打字上屏正常；重启后零连接失败。
- [x] **49 号 ③-1/③-3 删本地模式——远端唯一形态（2026-09-30）**:
  - 删除：`Config::use_engine_server` 开关字段（serde default，旧 config.json 残留行
    静默忽略）、`iuv-tsf` engine_host.rs（进程内引擎/词库加载）、daemon_client.rs
    （684 行：SHM 读取/旧管道/ensure_daemon）、全部 `use_server()` 分支（route_key/
    dispatch/langbar/mode/daemon_host/text_service）、设置页 `remote_mode` 判定
    （用户库面板恒从文件重载）、m10-deploy 的 A/B 指引。净删约 1300 行。
  - `REMOTE` OnceLock 语义同步修正：失败路径不再 `set(None)` 占死单例槽（首连
    失败可重试，见上条）。
  - **测试**：iuv-tsf 38 全绿；core/server/win 的失败均为存量 os error 5 环境
    红改动前后一致；clippy 四 crate 零警告。
- [x] **② 剩余真机回归 + 两个交互闭环（2026-09-30，18:12/18:41 两轮部署）**:
  - ② 遗留回归项用户实测通过：工具栏按钮/全局热键四态翻转（transport Ctl 往返）、
    语言栏菜单（设置页/工具栏开关/文案）、候选窗悬停（高亮正常无漏斗）、设置页
    （用户库可见/主题热载）；「混合模式调权」项随 ③-1 删本地模式作废。
  - **点击选词闭环（原已知缺口）**：服务端候选窗 WM_LBUTTONDOWN → 后台线程经
    ConnSender 发 `S2C::Ctl(CandidateClick(row))` → 客户端 TSF 线程 apply_ctl_cmd
    以 `Digit(row+1)` 走远端会话（与数字键同语义）。proto/iuv-win CtlCmd 各加
    CandidateClick(u8)（codec 序数 0x05）；客户端 ServerReq 独立线程处理无死锁。
  - **焦点切换候选窗同步**：`C2S::CandwinHide`——OnSetFocus/OnKillThreadFocus 时
    客户端通知 server 隐藏候选窗，**会话保留**（「焦点切换不打断会话」原则不变），
    回焦后下键经 sync_candwin 重显。
  - **composition 终止即收尾（脑裂修复）**：切焦点时 TSF 外部终止 composition，
    原逻辑客户端槽清空而 server 会话仍活——切回后首键被降级吞掉且远端会话残留
    （真机实锤：notepad 终止通知后必跟「降级丢弃会话」）。修复：Composition 挂
    on_terminated 回调，终止时立即清 last_effect + EndSession；dispatch_outcome
    对「composition 已死」的在途键兜底同步收尾。**部署后日志验证**：4 次终止
    通知后零降级、切回打字即刻正常（13-46ms）。
  - 遗留待办：设置页用户库**单条删除**入口（新功能，另立任务）；语言栏右键菜单
    在部分程序不弹出（事件未达 iuv 按钮，非本轮引入，待复现程序名定位）。
- [x] **49 号 ③-2 镜像归一——共享类型沉底 iuv-data（2026-09-30，四子提交）**:
  - **决策**：proto↔core 平行类型全仓收敛为 iuv-data 唯一定义（依赖指向最稳定的
    共享词汇层）；四态枚举统一 proto 系命名 ImeMode/ImeWidth/ImeScript/ImePunct
    （core 的 InitialMode/WidthMode/ScriptMode/PunctMode 为历史名，59 处机械改名）；
    CtlCmd/CtlResult（proto↔win 第三对镜像）一并下沉，CtlResult::Err 统一带 msg
    （客户端应用失败原因透传，替代 server 端写死文案）；Candidate/Effect 保留
    proto 瘦身版（49 §4.5.3 协议设计，非欠债）。
  - **iuv-data**（`dcf97ee`）：新增 `ime.rs`（四态四枚举 + ImeState + [u8;4] 唯一
    线编码自 core runtime.rs 迁移 + CtlCmd/CtlResult）、`key.rs`（Key 全 33 变体 +
    name/from_name + SessionEnd + PageInfo 统一 u32 定宽）、`candidate.rs`
    （CandidateKind + for_word）；UserMutation 迁入 userdict.rs 紧邻唯一消费者
    UserDict + `UserDict::apply_mutation` 单方法；serde 进 data 依赖。
  - **core/proto**（`9fcb7d7`）：core key.rs 仅剩 Effect、candidate.rs 仅剩
    Candidate、config/enums.rs 删四态、runtime.rs 整删（类型与线编码沉底）；
    proto msg.rs 删九组镜像定义（481→333 行）改 `pub use iuv_data`，WireImeState/
    WireSessionEnd 别名失效删除；PROTO_MIN/MAX 1→2（Err 带 msg 线格式变更；
    Key/UserMutation/四态变体声明序逐项核对不变，postcard 序号兼容）。
  - **转换函数退役**（`840a76e`）：server 删 core_key（33 臂）/core_user_mutation/
    core_ime_state/wire_session_end/wire_candidate_kind/proto_ctl_result +
    dispatch_ctl 内联 CtlCmd 镜像 match；tsf 删 wire_key（33 臂）/wire_ime_state/
    core_session_end/core_page/proto_to_win_ctl_cmd/win_to_proto_ctl_result；
    win ipc/msg.rs 本地 CtlCmd/CtlResult 改 re-export（codec 手写 tag 序不受影响）；
    core_candidate 零填充投影语义保留（客户端不需要 code/weight/seg_len）。
    全部跨界点直通：Key Copy 传值、ImeState/UserMutation/CtlResult 原样。
  - **验证**：workspace 编译零警告；clippy --all-targets 零警告；proto 18（含 Key
    全变体 roundtrip = 线格式不变证明）/tsf 38/hot_path 12/engine_session 97/
    transport 7 全绿；红项（core 12+3、data 6+13、server lib 7、win lib 3、
    transport 1、ui 1）经 stash 回上一提交逐项对照 = 存量环境红（os error 5 及
    同源 temp 文件类），改动前后一致。
  - **遗留**：iuv-win `ipc/msg.rs::Request`（Swap/Set/Remove/Block，a_adj 命名的
    UserMutation 第三镜像 + 手写 codec）：②迁移后数据面疑似死代码，待核实旧管道
    存活消费者后另立删除任务；③-3 混合模式过渡代码与 49 号任务书终稿收尾。
- [x] **iuv-server 单实例守卫（2026-09-30，③-2 部署时真机暴露）**:
  - **现象**：m10-deploy 后进程表 3 个 iuv-server（3 秒内相继拉起）——计划任务
    Start-ScheduledTask 与多个 TSF 客户端首连失败路径 spawn_server_process 并发；
    命名管道支持多实例创建，无守卫即多 server 瓜分连接（工具栏/引擎分家）。
  - **修复**：main 入口 CreateMutexW("iuv-server-singleton")——持有者存续 = 进程
    生命周期，退出自动释放；后来者记日志即退（守卫前置，不浪费引擎加载）。
  - **验证**：重部署后进程表恰 1 个 iuv-server。
- [x] **③-2 部署 + 第一轮真机日志回归（2026-09-30 21:10/21:15 两轮部署）**:
  - 部署即暴露 **server 多实例竞态**（计划任务 + 多客户端首连拉起，3 秒抢出 3 个；
    败者热键注册 0x80070581 全灭）→ 修：main 入口 CreateMutexW 单实例守卫
    （`0e6ec51`），重部署后恰 1 个，守卫两度正确拦截后来者（21:20/21:21 各一条）。
  - **打字链路全通**：tsf 侧 246 key / 21 commit（notepad 主测）；server 侧工具栏
    显隐/焦点跟随正常，全局热键 3/3 注册成功；ctl 路径 5 次四态翻转正常；慢键
    51 条 max=58.9ms P50=26.1ms（远低于 300ms 截止）。
  - **预期内混合期现象**：旧 DLL 进程（ZCode/msedge/taskmgr/leigod，部署前启动、
    旧 DLL 仍映射）连新 server 报 VersionMismatch {client:1, server:2}——PROTO
    1→2 的破坏性协商按设计拒绝混合，重启进程即恢复（新进程加载新 DLL）。
  - **存量确认**：[follow] ITfSource QI 失败（OnSetFocus）部署前 31424 条/部署后
    18 条 = 存量问题，与 ③-2 无关。
- [x] **③-2 注销重登回归（2026-09-30 21:26，全进程新 DLL）**:
  - server pid 1264 登录即就位（引擎 105ms），热键 3/3，工具栏焦点跟随正常；
    全进程首连 **0ms**（Explorer/msedgewebview2/notepad/WorkBuddy/ZCode/taskmgr/
    DeepSeek Harness 七进程，零 VersionMismatch 零失败零降级——PROTO bump 混合期
    随注销翻页结束）；tsf 侧 146 key / 27 commit / 9 ctl 正常，慢键 max 52ms。
  - 异常仅存量类：ITfSource QI 失败（E_NOINTERFACE，ZCode/WorkBuddy 等，部署前
    31424 条同源）+ ZCode GetTextExt 重定位失败走"沿用旧光标"兜底。③-2 回归通过。
