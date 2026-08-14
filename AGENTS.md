# iuv 输入法（代号 iuvim，谐音"哎哟喂"）

Rust + TSF 的 Windows 中文输入法。核心卖点（M2 起）：**用户掌控排序**——静态词频序默认稳定
（肌肉记忆安全）+ Alt+←/→ 主动调权（绝对值覆盖，反复调整收敛，见 `docs/plan/18-m2-user-dict.md`）。
M1（当前里程碑）：最小可用的全拼输入法。

## 当前状态

- [x] M1 最小 MVP：全拼打字链路（见 `docs/plan/00-overview.md`）——**已结案**（2026-08-09：手测 1-8 项通过、词库缺失透明模式通过）
  - 已知问题：Alt+Tab 切窗口时未确认的预编辑会残留上屏（TSF 终止 composition 的标准语义，微软拼音同款行为；残留为汉字首选而非拼音原文）——M3+ 或按需处理
  - **已知 bug（2026-08-11，已修）**：续接（选中间级词）后尾巴 commit 失败 `0x8000FFFF (E_UNEXPECTED)`。
    根因：选中间词走「EndComposition 上屏已选词 → 紧接 StartComposition 重建尾巴」，重建的 composition
    被 TSF 在应用（notepad 实测）的下一个 edit session 里终止（日志 `composition 终止通知`），而
    `OnCompositionTerminated` 不清理 → 后续 GetRange/EndComposition 永久失败。
    **修复方案（悬空状态）**：选中间级词不再产生任何 commit 信号——`part_commit` 契约字段删除；
    已选词悬空入栈，预编辑混合显示（`床前ming'yue'guang`），composition 全程单个、只做 set_text 全量更新，
    End→Start 窗口不存在 → bug 不复现；Esc 语义改为有已选词时上屏已选词；`OnCompositionTerminated` 兜底
    清槽+置终止标志，TSF 侧检测后丢弃会话降级重建。改动：session.rs/key.rs（Effect 删 part_commit）、
    session_bridge.rs、composition.rs（sink 共享槽）、text_service.rs（降级）、测试/契约/文档同步。
- [x] M1.5 候选策略对齐微软（2026-08-12 落地）：**三路路由**——单段档（`c`/`sh`/`shi` 纯单字，
  首字母桶 `initial_top`）；多段全完整档（现状全拼 k-loop 不动）；多段纯简拼档（`nh`/`nhm`/`nhmsx`
  构建期简拼键逐级砍尾巴，纯词、任意长度、部分消费尾巴续接复用悬空机制）；多段混拼档（`nhao`
  简拼段运行时展开音节笛卡尔配对，单级 ≤2000 查询剪枝）。数据层：dictc 对 ≥2 音节词生成简拼键
  （同表混存，路由隔离，IMEDIC01 格式零改动、新旧词库双向兼容）+ `Dict::initial_top` 首字母桶
  （每字母 top-500 词频序）。依据：`docs/research/msime-probe-checklist.txt` 微软实测清单
  （A~H 全组）。已知差距（M2+）：候选翻页与每页 5 个样式、符号/emoji/学习候选、翻页键自定义；
  排序用白霜词频与微软有数据级差异（M2 学习自适应）。
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
- [ ] 大写保形进序列（2026-08-14 实现待测）：Shift/CapsLock 字母 → `Key::ShiftChar` 大写原样进 raw
  （匹配只认小写：大写不被音节表命中、按不可匹配字符处理，`niHAO` 候选仍从 `ni` 前缀出；
  commit 原样上屏 `niHAO`/`Hello`）；字母大小写 = Shift 与 CapsLock 的 XOR（CapsLock+Shift 反转小写）；
  **大写同样是开会话键**（`is_session_start_key` 字母即开会话，`Hello` 的 H 进序列而非直接上屏）。
  改动：key.rs/session.rs（ShiftChar 臂）、keymap.rs（is_session_start_key 单条件）、
  session_bridge.rs（map_key XOR）、text_service.rs（capslock_on 传参）。
- [ ] **M2 主动调权 + 用户词库（2026-08-14 实现待测，分支 feat/m2-user-dict）**：Shift+←/→ 与页内相邻
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
- 后续：M2 钉选/屏蔽词交互（用户库段类型已预留）· M3 整句增强(LMDG)/模糊音 · M4 Tauri helper（WebView 候选窗+设置） · M5 安装器/词库导入/x86
- **中英切换已改系统机制（2026-08-12）**：`OPENCLOSE` compartment 真相源（系统"输入法/非输入法切换"热键驱动，
  OnChange 统一响应；语言栏点击归一写 compartment；Shift 切换已移除；激活即打开）。前置条件：用户在
  高级键设置把"输入法/非输入法切换"设为 Ctrl+Space（"切换输入语言"热键让位，Win+Space 仍可用）。
  已知遗留（未修）：有活动候选时按热键关闭，会话清理路径存在小 bug（2026-08-12 手测记录，待修）。

## 开发入口

**先读 `docs/plan/01-contract.md`（共享契约，接口唯一权威来源）**，再读对应模块任务书。
执行流程（W0 骨架 → W1 并行实现 → W2 组装）见 `docs/plan/00-overview.md` §3 与 `20-assembly.md`。

## 结构

| 路径 | 说明 |
|---|---|
| `crates/iuv-data` | 词库编译器 dictc + 二进制格式 + Dict 查询层 |
| `crates/iuv-core` | 引擎：切分/候选生成/unigram Viterbi/会话状态机/排序管线（跨平台纯 Rust） |
| `crates/iuv-repl` | CLI 调试前端 |
| `crates/iuv-tsf` | cdylib：COM/TSF 管线 + GDI 候选窗 + 语言栏"中/英"切换图标（Windows） |
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
