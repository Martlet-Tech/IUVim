# 35 平行类型/重复状态排查（2026-08-21）

背景：iuv-core 曾有 `InitialState`/`RuntimeState` 两结构体字段完全同构、靠机械 `From` 桥接（2026-08-21
已合并为单类型 `ImeState`，见 b30d2ec / 34-review §P3.3）。本报告全仓排查同类"多个 agent 各做一个
自己的类型"情况。**本轮只记录不修改**（2026-08-21 用户决策：只出报告），修复方向见 §5，执行另排期。

## 0. 依赖图（判断合并方向用）

- `iuv-core → iuv-data`
- `iuv-ui → iuv-core`
- `iuv-win → iuv-data + iuv-ui`（对 iuv-core 仅**传递**依赖，无直接边）
- `iuv-daemon → core + data + ui + win`
- `iuv-tsf → core + data + ui + win`

## 1. 确定重复

### D1. daemon 的 `theme`/`candidate_orientation` 用 String，重复 iuv-core 枚举

| 项 | 内容 |
|---|---|
| A | `platforms/windows/iuv-daemon/src/config.rs:15` `pub theme: String`、`:17` `pub candidate_orientation: String` |
| B | `crates/iuv-core/src/config/enums.rs:23` `ThemeChoice{Light,Dark}`、`:6` `Orientation{Vertical,Horizontal}`（serde lowercase 枚举） |
| 重叠度 | 语义 100% 相同；daemon 手写白名单校验（config.rs:71-80 `if t == "dark" || t == "light"`），新增枚举值时 **daemon 侧静默丢值**（else 忽略）、core 侧 serde 报错 |
| 桥接 | 无类型桥接，字符串双端各自解析；daemon `current_theme`（toolbar/mod.rs:393-398）String→调色板 match |
| 结论 | **确定重复（降级型）**：daemon 已依赖 iuv-core（`initial_state: ImeState` 就在同一结构体），可直接用枚举 |

### D2. 4 组工具函数双份

| 函数 | 位置 1 | 位置 2 | 说明 |
|---|---|---|---|
| 配置路径 | `iuv-core/config/io.rs:78` `default_config_path()`（pub） | `daemon/config.rs:48` `config_path()` | 函数体逐字相同 |
| JSONC 注释剥离 | `iuv-core/config/io.rs:91` `strip_jsonc_comments`（pub(crate)） | `daemon/config.rs:206`（私有） | ~35 行逐字相同；core 是 pub(crate)，daemon 跨 crate 用不了只能抄 |
| 旧键 `english_punctuation` 迁移 | `iuv-core/config/io.rs:60` `migrate_initial_state` | `daemon/config.rs:86-93` load 内嵌 | 语义双实现，**行为已不一致**（实现时核对收敛：core 缺 `initial_state` 才读旧键，daemon 同语义但写路径清理逻辑另起） |
| ThemeChoice→调色板 match | `tsf/text_service.rs:132-135`、`:389-392`、`com/daemon_host.rs:56-59` | `daemon/toolbar/mod.rs:393-398` | 同一映射重复 4 处 |

## 2. 高度重叠（有桥接，类 InitialState→RuntimeState 模式）

### H1. `DaemonConfig` vs `iuv_core::Config`——同一 config.json 两套结构

| 项 | 内容 |
|---|---|
| A | `daemon/config.rs:13` `DaemonConfig`：theme(String)/candidate_orientation(String)/page_size/initial_state(ImeState)/passthrough_apps/candidate_owner_apps/disabled_log_modules（7 字段） |
| B | `iuv-core/config/mod.rs:27` `Config`：page_size/max_candidates/max_word_syllables/keymap/candidate_prefix/candidate_orientation(Orientation)/initial_state(ImeState)/passthrough_apps/candidate_owner_apps/theme(ThemeChoice)/disabled_log_modules（11 字段） |
| 重叠度 | **7 字段中 6 个同构**（theme/orientation 类型降级）。DaemonConfig 是 Config 子集，靠"读原 JSON 补丁式写回保留未知字段"（config.rs:129-136 注释明说保留 keymap/max_candidates）绕开缺失 4 字段 |
| 桥接 | 无 From；`load_config`（:57-113）逐字段手工挖 Value；`save_config`（:121-202）逐字段手工 patch |
| 结论 | **最接近刚合并的 InitialState/RuntimeState 案例**：同一磁盘文件两套结构 + 手工字段拷贝。修复方向：DaemonConfig 内嵌/复用 `iuv_core::Config`，load 走 `Config::load()` 投影，save 走 serde 序列化合并 root 保留未知键 |

### H2. TSF 中英模式双真相源（**真 bug 风险，最优先**）

| 项 | 内容 |
|---|---|
| A | `tsf/com/text_service.rs:95` `english_mode: Arc<AtomicBool>`（= ImeState.mode 的 bool 投影，true=英文） |
| B | `tsf/com/text_service.rs:110` `runtime: Arc<Mutex<ImeState>>` 的 `.mode: InitialMode`（runtime.rs:22） |
| 重叠度 | 100% 语义相同，都镜像 OPENCLOSE compartment；唯一同步写入点是 `mode.rs:42-56 apply_openclose`（同时写两处，注释自述"镜像"） |
| 读取点 | `english_mode`：key_routing.rs:63、mode.rs:44/47/119/155、langbar.rs:242-243/393/402；`runtime.mode`：会话 live 读 + mode.rs:51 |

**已核实的两条发散路径（fallback 只写一端）**：
1. `text_service.rs:238-243` `apply_ctl_cmd` fallback：写 OPENCLOSE 失败只 `runtime.set_field(CTL_FIELD_MODE, value)` + `after_runtime_change()`，**不更新 english_mode** → 按键路由（key_routing.rs:63 读 english_mode 判英文直通/全角）立即与运行时态分叉
2. `langbar.rs:242-245` `toggle_mode` fallback：compartment 缺失/写失败只翻 `self.mode`（=english_mode 共享 Arc），**不更新 runtime.mode** → 会话 live 读分叉
3. 构造期隐患：`text_service.rs:171` `english_mode=false` 而 `runtime.mode`=config.initial_state（可 English），Activate 前窗口期不一致

**修复方向（Phase A）**：删 `english_mode`，单源 `runtime.mode`；新增 `is_english()` helper（lock runtime 判 mode）；key_routing.rs:63/mode.rs:44/47/119/155 改走 runtime；`LangBarItemButton` 改持 `Arc<Mutex<ImeState>>`（text_service.rs:394 传 `self.runtime.clone()`），toggle fallback 写 `runtime.set_field` + 经回调触发 `after_runtime_change`；构造期天然单源。性能：引擎会话每键本就 lock 同一 Mutex（effect/to_output live 读），key_routing 多一次 lock 可忽略。

### H3. 四态三重表示：`ImeState`/`ToolbarState`/`ToolbarSpec`

| 类型 | 位置 | 编码 |
|---|---|---|
| A `ImeState` | `iuv-core/config/runtime.rs:20` | 枚举 mode/width/script/punct |
| B `ToolbarState` | `iuv-win/ipc/msg.rs:63` | u8×4（管道线格式） |
| C `ToolbarSpec` | `iuv-ui/toolbar.rs:50` | u8×4 + hover/pressed（渲染输入） |

桥接：`ImeState::to_toolbar()`→(u8,u8,u8,u8)（runtime.rs:34-53）→`From<(u8,u8,u8,u8)> for ToolbarState`（msg.rs:70-80）；`set_field`↔`ToolbarState::field`↔`CTL_FIELD_*`；`ToolbarState`→`ToolbarSpec` 逐字段拷贝（daemon toolbar/window.rs:201-209）。

结论：A→B 的 u8 线格式是**有理由的**跨进程边界（iuv-win 不依赖 iuv-core 是故意，保持 core 纯 Rust）；**C 的 u8 字段无理由**重复 B——iuv-ui 依赖 iuv-core，`ToolbarSpec` 可直接收 `ImeState`（daemon 侧 ToolbarState→ImeState→ToolbarSpec，daemon 同时依赖 win+ui+core）。

### H4. `UserMutation` ↔ `Request` 写变体逐字段同构

| 项 | 内容 |
|---|---|
| A | `iuv-core/userdict.rs:15` `enum UserMutation{Swap{a_code,a_word,a_eff,b_code,b_word,b_eff},Set,Remove,Block}` |
| B | `iuv-win/ipc/msg.rs:6` `Request` 的 4 个写变体，与 A 逐字段同构（仅 `a_eff`/`a_adj` 命名不同） |
| 桥接 | 机械桥 `user_mutation_to_request`（`tsf/daemon_client.rs:360-391`）逐字段 clone 拷贝，注释"与 UserDict 方法一一对应"；无 `impl From`，散落在第三个 crate |

修复方向（Phase C）：iuv-win 加 iuv-core 直接依赖（已传递依赖），`impl From<&UserMutation> for Request` 替代手工桥，从 tsf 挪到 iuv-win 与 Request 同处。

## 3. 正当分层（保持不动）

- `iuv-ui::Theme`（调色板 12 字段）vs `iuv_core::ThemeChoice`（二选一开关）：**选择 vs 渲染数据**，不合并
- `iuv_ui::UiSnapshot` vs `iuv_core::Effect`/`Candidate`：投影函数（effect_to_snapshot），非字段拷贝桥
- `iuv_core::Key` vs `iuv_tsf::KeyAction`：KeyAction 是路由判定结果，内部**持有** Key，非重复定义
- `CaretRect`/`Rect`/`Area`：已收敛，无重复
- `SettingsApp`（daemon/settings.rs:168）UI 编辑缓冲：标准 UI 模式（egui String 单选/多行文本 vs 持久结构）
- `iuv_data::UserDict` vs `iuv_core::UserState`：装配元数据，异构
- `iuv-repl`：零自定义类型，全用 iuv-core

## 4. 结论汇总

| 优先级 | 项 | 性质 | 修复 |
|---|---|---|---|
| P0（真 bug） | H2 | TSF 中英模式双真相源，2 条 fallback 已分叉 | 收敛到 runtime.mode 单源 |
| P1（确定重复） | D1 | daemon String 版 theme/orientation | 改 `ThemeChoice`/`Orientation` |
| P1（确定重复） | D2 | 4 组工具函数双份 + 迁移漂移 | 收敛 iuv-core/iuv-ui 单源 |
| P2（高度重叠） | H1 | DaemonConfig vs Config（同文件两套结构） | DaemonConfig 复用 `iuv_core::Config` |
| P3（机械桥） | H3 | ToolbarSpec u8 重复（ToolbarState 线格式有理由） | ToolbarSpec 收 `ImeState` |
| P3（机械桥） | H4 | UserMutation↔Request 手工桥 | iuv-win 侧 `From` |
| — | 正当分层 | Theme/ThemeChoice、UiSnapshot、KeyAction、SettingsApp、CaretRect | 不动 |

> 依赖边改动：H3/H4 需要 **iuv-win → iuv-core 直接依赖**（当前为传递），方向合法（iuv-tsf/daemon/ui 均依赖 core；core 不依赖 Windows，哲学不受损）。P3.1 刻意拆掉的是 **iuv-core → iuv-win**（反向），不受影响。

## 5. 建议修复阶段（未执行，另排期）

- **Phase A（H2）**：删 `english_mode`，单源 `runtime.mode`；`is_english()` helper；LangBarItemButton 改持 `Arc<Mutex<ImeState>>`；三处写入点（apply_openclose / ctl fallback / langbar fallback）收敛同一函数。
- **Phase B（D1+D2+H1 一次重构）**：DaemonConfig 复用 `iuv_core::Config`（theme/orientation 枚举化、load 改 `Config::load()` 投影、save serde 合并保留未知键、迁移收敛 core 单源）；`iuv_ui::theme_for(ThemeChoice)` 收敛 4 处调色板 match；删 daemon 版 `config_path`/`strip_jsonc_comments`。
- **Phase C（H3/H4）**：iuv-win 加 iuv-core 直接依赖；`From<ImeState> for ToolbarState`（删裸元组中转）/`From<&UserMutation> for Request`；`ToolbarSpec` 收 `ImeState`。

每阶段 `cargo test --workspace` + iuv-tsf/iuv-daemon 双 release 全绿再进下一阶段；行为保持（daemon 设置页需手测）；不动词库格式/TSF 协议/引擎算法。