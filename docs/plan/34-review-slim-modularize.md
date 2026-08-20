# 34 代码审查：瘦身 + 模块化（2026-08-21）

迭代多轮后首次全量审查（7 crate，~2.2 万行）。目标：**瘦身**（死代码/冗余）+ **模块化**（god file 拆分/重复样板收敛）。
执行：分三阶段，每阶段 `cargo test --workspace` + 双 release 构建全绿再进下一阶段。改完 dev-deploy 手测，**不提交**。

## 基线

`cargo test --workspace` 全绿为每个阶段的回归基准。

## P1 瘦身（低风险）

- **P1.1 iuv-core**：删 `rerank.rs`（RerankCtx/RerankStage/StaticOrder，生产恒跑 no-op，M2 已绕过）+ `store.rs`（UserDataStore/NullStore，生产用 NullStore，M2 学习模型已废弃）；engine `stages`/`store` 字段/`record_selection` 钩子删除；`with_parts` 签名减两参；删 `RuntimeState::from_toolbar`/`Config::is_page_key`/`Keymap::page`/`Engine::lookup`/`Engine::user_dict`/`viterbi::best_sentence`/`ScriptConverter::empty`；`OOV_PENALTY`→`pub(crate)`；lib.rs 死 re-export 修剪；删 rerank 专项测试
- **P1.2 iuv-data**：删 `OCC_MAGIC`/`SHM_*`/`encode_decode_ctl_*` re-export、`CtlServer::interrupt`；`to_frame`/`parse_frame`→`pub(crate)`，`PIPE_*`/`CTL_PIPE_PREFIX`→私有
- **P1.3 iuv-tsf**：删 `Composition::is_active`/`NullCandidateUi`/`CandidateUi::move_to`/`CandwinCandidateWindow::default`/`MenuWindow::is_visible`/`DaemonClient::toggle_toolbar`/`DISPLAY_ATTR_GUID`/`CtlEndpoint::new` 死参数；`process_id`/`thread_id` 与 log.rs 去重；可见性收紧；candwin_demo 直引 iuv-ui
- **P1.4 iuv-daemon**：删 `settings_ctx`（只写不读）、`ToolbarHost::quit`（写而不读）；`ToolbarPref`/`load_pref`/`save_pref` 降可见性；修 1024×800 过期注释
- **P1.5 iuv-ui**：prune 11 项死 re-export；`render_menu`/`render_tooltip` padding 字面量引 `layout::PAD_*`
- **P1.6 helper 抽取**：`Candidate::for_entry`（5 处）/去撇号（3 处）/按文本去重（3 处）/`Session::selected_idx`（4 处）；**Config 克隆缓存**（每次按键 ~4-5 次整份克隆 → engine 缓存 page_size）

## P2 模块化（行为保持拆分）

- **P2.1 iuv-core**：`engine.rs`(1178)→`engine.rs` + `userdict.rs`（M2 用户库写路径）+ `routes.rs`（Route/五档候选生成）；`config/mod.rs`(716)→`enums.rs`/`runtime.rs`/`io.rs`/`mod.rs`
- **P2.2 iuv-tsf**：`text_service.rs`(1249)→`engine_host.rs`/`key_routing.rs`/`mode.rs`/`daemon_host.rs`/`dispatch.rs`，text_service 剩 COM 壳 ~500 行
- **P2.3**：抽纯函数 `route_key(...) -> KeyAction`，`test_key_down`/`handle_key_down` 共用（消灭 ~60 行对称复制）
- **P2.4 iuv-ui**：`render.rs`(1092)→`render.rs`/`toolbar.rs`/`paint.rs`；`render_candidate` 返回 `(Surface, Vec<Rect>)` + `candidate_label` pub → 删 `candwin::compute_rows`
- **P2.5 iuv-win**：新增 `popup.rs`（LayeredWindow：类注册/创建/DPI/GWLP_USERDATA/wndproc 默认臂/Drop），candwin/menu_window 各瘦 ~200 行；**daemon 工具栏未改**（bar/tip 双类各自 wnd_proc + 类注册与建窗解耦的线程结构，与 LayeredWindow「create 即注册即挂接」流程不合，其瘦身由 P2.6 拆分承担）
- **P2.6 iuv-daemon**：`toolbar.rs`(1183)→`toolbar/{mod,window,tooltip,prefs}.rs`（mod.rs 即 host）；`daemon_client` 的 `send_request`/`pipe_online` 合并 `request_once`（`silent_connect` 区分静默存活探测 / 记日志写请求）

## P3 架构移动（iuv-data 恢复跨平台）

- **P3.1** 解耦 iuv-core→ToolbarState：`to_toolbar()` 返回 `(u8,u8,u8,u8)`
- **P3.2** `ipc.rs`+`shm.rs`（1673 行，占 iuv-data 44%，纯 Windows）移入 iuv-win（`ipc/{msg,codec,pipe,ctl}` + `shm.rs`），消费方改 import，依赖边零新增；iuv-data 保留 mmap（有 `fs::read` 真降级）
- **P3.3** `InitialState`/`RuntimeState`（字段同构、机械 From + 重复 Default）合并单类型 `ImeState`（Copy + serde + `#[derive(Default)]`）；`session.rs`/`text_service.rs` 构造点改 `config.initial_state` 直取（Copy）；JSON schema 不变

## 风险控制

- 每阶段独立验证；拆分全部行为保持，靠 3000+ 行集成测试兜底
- 不动词库格式、不动 TSF 协议、不改引擎算法
- 文件属主矩阵属原并行开发约束，本轮不并行，忽略