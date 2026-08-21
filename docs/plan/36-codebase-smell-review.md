# 36 - 全工作区代码坏味道审查（2026-08-21）

> 审查范围：全部 7 个 crate 的 `src/`（~21k 行，含 2.6k 测试）。
> 审查方式：daemon/win 由子代理完成，core/data/tsf 亲审，高危项全部亲自复核。
> 三类坏味道判定标准：①冗余分支/if 补丁 ②跨文件重复 ③散落变量应聚合结构体。
> 本文只记录调研结论与分批重构计划，未做任何代码改动。

## 总体评价

**核心链路质量高**：key_routing.rs 的 `route_key` 单点判定 + KeyAction 枚举消灭了
Test/Handle 对称复制；userdict core/data 分工清晰（core=引擎写路径 impl，data=存储格式）；
composition.rs 悬空槽机制干净；routes.rs 五档路由有契约对应。

问题集中在三个"后写"区域——**daemon 工具栏、配置读写、日志**，以及少量跨 crate 平行映射。
根因模式：多智能体并行开发时，后写的模块没有发现先前的收敛层（popup.rs / iuv-core config），
自己又造了一份。

---

## 一、冗余分支 / if 补丁

| # | 位置 | 问题 | 严重度 |
|---|---|---|---|
| P1 | `iuv-daemon/main.rs:263` | 写请求分派 `_ => unreachable!("上方已处理")` —— 未来加 Request 变体漏改此处 = 每请求 panic（违反 daemon 绝不 panic 纪律），失败表现为连接无声断开难排查 | **高** |
| P2 | `iuv-tsf/daemon_client.rs:253-293` | `pipe: Mutex<Option<PipeClient>>` 缓存是死机制——成功/失败/重连后一律清空，字段永远以 None 进入下次调用，"缓存"只剩 Mutex 开销 + 三层重连补丁链 | 中 |
| P3 | `iuv-daemon/toolbar/window.rs:247-253` | 齿轮按钮绕管道回环触发 OpenSettings（自己连自己的服务端再回到同一标志位），而 `state.open_settings` 就在手上；多出连接失败静默丢弃与管道忙两种失败态 | 中 |
| P4 | `iuv-tsf/com/mode.rs:78`、`iuv-core/session.rs:288-290` | 过时注释：仍称 flush_session/pending_text "焦点切换 Alt+Tab 共用"——2026-08-21 设计变更后焦点切换不再 flush，误导后续开发 | 中 |
| P5 | 魔法数字簇：`daemon/main.rs:192`(200ms)、`state.rs:124-131`(0..3×50ms)、`toolbar/mod.rs:133`(5s)、`prefs.rs:43`(-10000/40000)、`toolbar/mod.rs:414,429`(32767×2)、`tooltip.rs:61`(+8/+12) | 内联字面量无依据注释 | 低 |
| P6 | `toolbar/window.rs:131-141,388-395` | 同一锁连续两次获取（两段之间可插入其他线程修改，TOCTOU） | 低 |
| P7 | `daemon_client.rs:252`(silent_connect bool 只控日志)、`tooltip.rs:42`(两个下划线死参数仍被传值) | bool 参数 / 死参数 | 低 |
| P8 | `Response::Err` 生产环境不可达（daemon 从不构造，仅 codec 与测试出现），`daemon_client.rs:235-238` 拒绝降级分支恒假 | 协议预留无害但读者会误以为有拒绝语义 | 低 |

## 二、跨文件重复

| # | 位置 | 问题 | 严重度 |
|---|---|---|---|
| D1 | `iuv-daemon/config.rs` ↔ `iuv-core/config/io.rs` | **DaemonConfig 手搓镜像 core Config**：strip_jsonc_comments 逐字复制 ~34 行（config.rs:206 vs io.rs:91）、english_punctuation 迁移 shim 两份（config.rs:84-93 vs io.rs:60-74）、BOM 剥除两份、路径解析两份；theme/orientation 退化为 String 手验（与 core serde 枚举行为不一致）。新增一个配置字段要同步 ≥4 处（core 结构体/daemon 结构体/load 手工 parse/save 手工 insert） | **高** |
| D2 | `iuv-daemon/toolbar/*` ↔ `iuv-win/popup.rs` | 工具栏五件套整套重写 LayeredWindow：类注册（mod.rs:276 ≈ popup.rs:46）、建窗（mod.rs:300 ≈ popup.rs:71）、lparam 解包逐字节相同（mod.rs:334 ≈ popup.rs:137）、GWLP_USERDATA 取回（window.rs:413 ≈ popup.rs:160）、Drop 清零+销毁（window.rs:400 ≈ popup.rs:190）。candwin/menu_window 都复用了，唯独 toolbar 绕开；x86 指针宽度兼容（popup.rs:112）等修复不会传播，两份已开始漂移 | **高** |
| D3 | `daemon/log.rs` ↔ `iuv-tsf/log.rs` | denylist 过滤机制 ~80% 逐字相同（DISABLED OnceLock/set_log_modules_disabled/module_disabled，daemon:14-39 vs tsf:15-40）；log_line 仅差 pid/exe 名格式。26-log-modules 过滤语义改动必须双写 | **高** |
| D4 | `settings.rs:172-531` ↔ `DaemonConfig` | 设置页 7 字段双向平铺搬运：new() 逐字段拆入、apply() 先组装再成功后逐字段抄进 state.config 共两遍（settings.rs:512-531）。每加设置项动 6 处（结构体/load/save/UI 字段/new/apply×2），漏一处静默丢设置 | **高** |
| D5 | 四态字段序数（mode=0/width=1/script=2/punct=3） | 3 crate 平行映射：`config/runtime.rs:34-94`(to_toolbar/set_field) + `ipc/msg.rs:93-109`(ToolbarState::field + CTL_FIELD_*) + `codec.rs:146-151,379-390`(按固定序读写) + `toolbar/window.rs:255-270`(消费)。加第五态跨 3 crate 改 ≥7 处，编解码错位运行期才爆 | 中 |
| D6 | `%LOCALAPPDATA%\iuv` 基目录 env 回退链 **5 份** | `daemon/config.rs:49`、`daemon/main.rs:152`、`toolbar/prefs.rs:19`、`core/config/io.rs:79`、`tsf/com/engine_host.rs:110`（子代理报 4 份，亲审追加 tsf 侧第 5 份） | 中 |
| D7 | `win/ipc/pipe.rs:179-276` ↔ 自家 `imp` 模块(52-170) | imp 本为共享而建（ctl.rs 在用），PipeClient::connect/PipeServer::accept 又把 CreateFileW 循环/CreateNamedPipeW 序列内联一遍（各 ~35 行近乎逐字）；同一连接协议 4 份实体 | 中 |
| D8 | DPI scale 三份 | `toolbar/window.rs:88-106` ≈ `tooltip.rs:75-93`（逐字相同）≈ `popup.rs:118-134`（同逻辑异返回型 u32） | 中 |
| D9 | theme 转换 `match cfg.theme {Light=>theme_light(),Dark=>theme_dark()}` ×3 | `text_service.rs:132,389`、`com/daemon_host.rs:56` | 低 |
| D10 | 排序键 `b.weight.cmp(&a.weight).then(a.word.cmp(&b.word))` ×3 | `core/routes.rs:144,262`、`data/dict.rs:479`(merged) | 低 |
| D11 | 杂项小重复 | codec tag 字面量三处裸写（encode 79-141/decode 182-260/文件头表 4-30）；ULW 窗口过程公共臂 ×2（WM_PAINT/ERASEBKGND/MOUSEACTIVATE，window.rs:444 ≈ tooltip.rs:113）；圆角命中几何 daemon 复刻 iuv-ui 渲染参数（mod.rs:351 注释自认"与 render_toolbar 一致"，渲染参数一改穿透区就错位）；日志文件名清单跨进程硬编码（daemon log.rs:64 含 "iuv-tsf.log"，真名定义在 tsf/log.rs:108） | 低 |

## 三、散落变量（应聚合结构体）

| # | 位置 | 问题 | 严重度 |
|---|---|---|---|
| S1 | `tsf/com/dispatch.rs:34-41` | dispatch_effect 收 5 个 `Rc<RefCell<>>` 平行槽（session/composition/ui/caret/cand_elem）——TextService::dispatch 与候选窗点击回调两条路径都要拼这手 5 连参数，应聚合为上下文结构体 | 中 |
| S2 | `daemon/state.rs:26-32` 四个 AtomicBool | dirty/close_settings/open_settings/quit_flag 的转移规则散落 main/settings/toolbar 三文件且无属主方法（如 main.rs:127 消费 open_settings 前须先清 close_settings 的配对约定、Quit 同时置两标志） | 中 |
| S3 | `Config::load()` 散读 ×8 | text_service.rs 一个文件 4 次（new×2/activate×2）+ engine_host/daemon_host×2/repl，每次全文件读+BOM+JSONC 剥离+解析，无缓存单点 | 中 |
| S4 | `core/session.rs:256-274` | to_output→convert_script 链对 runtime 锁两次；effect() 对每个候选调 convert_script → N×(runtime 锁+script_converter 锁)，锁粒度碎 | 低 |
| S5 | `Shared`↔`toolbar.json` 平行持久化状态 | visible/pos 同步点手工散布 ToggleToolbar(mod.rs:184)/end_drag(window.rs:392) 两处 | 低 |

## 四、其他观察

- `data/opencc.rs:12-14` 格式注释写 `u16 key_len`，实现是 u32（read_str/read_u32）——文档失真
- `engine.rs:261` is_syllable_prefix 线性扫 407 音节表，classify 每键多次调用（量小不致命，可做前缀索引）
- `Engine.page_size: AtomicU32` 是 config 的手工派生缓存，set_config 忘记同步即漂移（现有代码正确，属脆弱点）
- session_bridge map_key(:82) 与 fullwidth_pending(:133) 各写一份 Shift⊕Caps 大小写惯例
- key_routing.rs char_code(vk) 在 route_key 内重复调用（MapVirtualKeyW 每次 syscall）
- 值得表扬：route_key 单点判定、userdict core/data 分工、composition 悬空槽、dictc 平面格式段表驱动

---

# 分批重构计划

原则：每批独立可验证、先零风险后动结构、全程 `cargo test --workspace` 兜底。
依赖关系：批 1 独立可先行；批 2/3/4/5 相互独立可任意顺序；批 6 建议最后。

### 批 1：零风险清理（约半天）
- P1 unreachable → `Response::Err { msg }`（顺带激活 L8 死分支）
- P4/P8 + opencc u16 + `iuv-win/lib.rs:7-8`("工具栏已收敛"失实) + `state.rs:25`("2s 定时器"已不存在) 过时注释修正
- P5 魔法数字提具名常量并注明依据；P6 合并临界区；P7 删死参数
- **验证**：cargo test 全绿 + grep 确认无残留

### 批 2：配置单点化（1 天，收益最大）
1. iuv-core 导出 `strip_jsonc_comments`/`default_config_path` 为 pub
2. DaemonConfig 派生 Serialize/Deserialize、theme/orientation 复用 core 枚举，load/save 走 serde_json::Value 补丁合并，删手搓四件套（迁移 shim/JSONC/BOM/路径）
3. SettingsApp 改 `{orig, draft: DaemonConfig}` 双份结构，控件直编 draft，apply = save_config(&draft) + 替换 state.config，删 6 处平行搬运
4. TSF 侧 theme 转换提 helper 一处（D9）
- **验证**：daemon config 测试迁移适配 + 手测设置页保存/热载/旧配置升级迁移

### 批 3：窗口样板收敛（1 天，需手测）
- daemon toolbar 五件套迁入 LayeredWindow（照抄 candwin 用法），删 mod.rs register_class/create_window/client_pos 与 window.rs get_bar_mut/Drop 清零
- DPI scale 收敛为 popup.rs 方法（D8）；ULW 公共臂下沉 popup.rs（D11 部分）
- 圆角命中几何改由 iuv-ui 导出 hit_test（D11 部分）
- **验证**：cargo check + 手测工具栏显示/拖动/tooltip/点击穿透 + x86 构建

### 批 4：IPC 清理（半天）
- P2 删 pipe 死缓存字段（request_once 直写 connect→request→drop，重连语义显式命名 with_retry_once）
- P3 齿轮直改 `state.open_settings`
- D7 pipe.rs 包装类改调自家 imp（各删 ~35 行）
- D11 codec tag 提具名常量
- D5 四态序数收敛：iuv-core 导出字段序常量（或 ImeState 派生读写），msg/codec 引用同源
- **验证**：cargo test + 双进程手测（工具栏四态切换/自造词同步/daemon 重启恢复重注册）

### 批 5：日志统一（半天）
- denylist 机制下沉 iuv-win（已有 set_logger 钩子承载转发），daemon/tsf 只留文件名与 pid/module 格式差异
- LOG_MODULES 设置页清单与 log tag 常量同源绑定（tag 改名编译期报错而非静默失效）
- 日志文件名常量共享（D11 clear_logs 跨进程耦合）
- **验证**：设置页开关各模块实测生效

### 批 6（可选）：core/tsf 微优化
- S1 dispatch 上下文结构体（5 个 Rc 槽聚合）
- S2 四标志改 mpsc\<Command\> 或封装 DaemonState 方法（request_settings/request_quit）
- S3 Config::load 收敛到 engine_host 单次缓存 + 传递
- S4 effect() 简→繁转换提到循环外锁一次
- D10 排序键抽比较函数；is_syllable_prefix 前缀索引；char_code 单次计算
- **验证**：cargo test + repl 冒烟
