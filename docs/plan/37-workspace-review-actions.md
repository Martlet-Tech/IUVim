# 37 - 全工作区健壮性审查与修复行动（2026-08-21）

> 审查范围：全部 7 个 crate 的 `src/`（21,335 行）。方式：data/tsf/ui/win/daemon 五域子代理审查 +
> core 亲审，高危项全部对照源码复核。
> 视角：八项清单（所有权借用 / 错误处理 / 性能 / 依赖 API / unsafe / 并发 / 平台兼容 / 风格架构），
> 本文只记录结论与分批修复计划，未做任何代码改动。
> 与 36 号文档分工：36 = 坏味道/重复/结构；本文 = **正确性/健壮性/红线**。重叠项标注「同 36-x」。

## 总体结论

工程质量显著高于平均水平：生产代码 unwrap/expect 极少（tsf 8 处、daemon/win/ui/core 合计 3 处，
全部有不变式支撑），SAFETY 注释覆盖率接近 100%，Mutex 免中毒写法全库统一，「绝不 panic 到宿主」
在按键主路径基本成立。**但存在 6 条可触达的红线级缺陷**（P0），集中在「加载期校验承诺与查询期
expect 不匹配」「32 位算术回绕」「非 ASCII 配置值重解释」三类根因。

---

## 一、P0 红线（违反「宿主进程内绝不 panic」或静默损坏用户数据）

| # | 位置 | 问题 | 根因 |
|---|---|---|---|
| R1 | `iuv-data/dict.rs:537,542` | `.expect("词库已校验 code 边界")` ——消息失实：加载期 `record_step`(:557-570) 只查字节长度**从不校验 UTF-8**。含坏 UTF-8 的词库通过全部加载校验，第一次命中该记录的按键即在宿主进程 panic | 加载校验与查询假设脱节 |
| R2 | `iuv-data/dict.rs:238-243` ↔ `:522-526` | 索引偏移只要求 `< records_len`，不要求落在记录边界。一次位翻转即可让加载通过、二分首次探测 `code_at` 切片 panic | 同上 |
| R3 | `iuv-win/ipc/codec.rs:357-364` | `Reader::take` 用 `pos + n > len` 判界——**32 位构建**（M7 含 x86）下恶意帧 `len≈0xFFFFFFFF` 回绕骗过检查 → 切片 panic 进宿主（`decode_response` 运行在 TSF 进程） | usize 回绕 |
| R4 | `iuv-core/config/io.rs:91-124` + `iuv-daemon/config.rs:206-239` | strip_jsonc_comments 逐字节 `u8 as char` 把多字节 UTF-8 按 Latin-1 重解释——配置值含中文（如 `微信游戏.exe`）保存后再加载即乱码固化，设置页回显乱码再保存即永久失效 | 字节≠字符 |
| R5 | `iuv-tsf/session_bridge.rs:73` | `char_code - 0x30` 下溢：AZERTY 上 VK_1 无 Shift 返回 `&`(0x26)，debug panic（guard 吞掉降级放行）/ release 回绕吞键——**数字排非 QWERTY 布局全坏**，唯一影响真实用户的功能缺陷 | 键盘布局依赖 |
| R6 | `iuv-tsf/text_service.rs:141-159` | 候选窗点击闭包捕获自身 `Rc` 强引用并存入同一对象的 `click` 字段 → 引用计数永不到 0：每实例泄漏一个候选窗；窗已建则 **HWND/DIB 永不销毁**，session/composition 等 4 个 Rc 被永久持有。Excel 类频繁 Activate/Deactivate 场景累积 | 自引用闭包 |

### P0 修法（逐项）

- **R1**：`record_step` 内对 code/word 字节补 `str::from_utf8` 校验（返回 Err 走既有 `bad()`）；
  两处 expect 从此为真。备选（不推荐）：expect 改跳过该记录——静默错数据比 panic 更糟。
- **R2**：加载期顺序扫描天然产出记录边界序列——把索引段偏移收集后排序，与扫描边界做**双指针
  归并校验**（零额外大分配，~1.25 万条 ×4B 排序缓冲可忽略）；不等即 `Err`。
- **R3**：`take` 改 `checked_add`：
  ```rust
  let end = self.pos.checked_add(n).ok_or_else(|| bad("长度溢出"))?;
  if end > self.data.len() { return Err(bad("载荷截断")); }
  ```
  附回归测试：32 位语义下构造 `str_` len = u32::MAX 的帧必须 Err 不 panic。
- **R4**：改为**字节级透传**实现（输出 `Vec<u8>`，仅 ASCII 引号/斜杠参与状态机，≥0x80 字节原样
  拷贝，末尾 `String::from_utf8`），core 侧函数升 `pub`，daemon 删除自己的副本改调 core（顺带消
  36-D1 的一半）。附测试：含中文值的 jsonc 往返不变。
- **R5**：映射臂加守卫 `if !(b'1'..=b'9').contains(&char_code) { 放行 }`，仿 OEM 键的 char_code
  校验惯例。附测试：char_code=0x26 时不得产生 Digit。
- **R6**：闭包改持 `Rc::downgrade`，回调内 `upgrade()` 失败即返回（窗已亡）；或在
  `CandwinCandidateWindow` Drop 中 `set_on_click(None)` 断环。推荐前者。

### P0 验证

`cargo test --workspace` 全绿 + 新增 4 个回归测试（R1 坏 UTF-8 词库拒载、R2 索引错位拒载、
R3 溢出帧 Err、R4 中文往返）+ 手测：dev-deploy 后 AZERTY 模拟（改键盘布局）数字上屏、
Excel 反复 Activate/Deactivate 无句柄增长（任务管理器 GDI 对象数）。

---

## 二、P1 高价值修复（按域分组）

### 并发 / 跨进程

| # | 位置 | 问题 | 修法 |
|---|---|---|---|
| C1 | `iuv-win/shm.rs:281-307` | seqlock 缺「拷贝后复核 version」：读到旧 version 后数据区正被覆写 → 新旧混杂字节，模块头「绝不见半新半旧」承诺落空 | 读侧改为：读 v1/data_len → 拷入局部 Vec → 重读 v2，v1≠v2 重试（上限 3 次）→ 从局部副本解析 |
| C2 | `iuv-tsf/text_service.rs:262-274` | `Drop` 第一行 `fetch_sub(INSTANCE_COUNT)`，之后才 join accept 线程 + 阻塞管道 IO——窗口内 `DllCanUnloadNow` 返 S_OK → 卸载 DLL 崩溃 | fetch_sub 移到 Drop 最后一条语句（或引入覆盖 ctl 线程的 busy 标志） |
| C3 | `iuv-win/ipc/pipe.rs:218-222` | 连接后 `ReadFile` 零超时零取消——daemon handler 挂死则 TSF 宿主打字链路永久冻结（ctl 通道有 CancelSynchronousIo，此管道无对称机制）；`TextService::drop` 内阻塞 IO 同样无超时 | read_frame 改 overlapped + 超时（镜像 ctl.rs 方案）；Drop 内 unregister 加「跳过网络操作」开关或短超时 |
| C4 | `iuv-tsf/ctl.rs:174-178` | Drop 与 accept 线程对同一句柄值双重 CloseHandle，句柄复用期可误关他人句柄 | accept 线程退出前置 `handle_slot=None` 跳过二次关闭 |
| C5 | `iuv-tsf/daemon_client.rs:152-166` | 在线判定基于「段能否打开」，daemon 死后段依然打开成功 → 重启后 on_online/重注册永不触发 | 在线状态只在管道请求成功/失败翻转处更新；段打开成功不再置 online |
| C6 | `iuv-daemon/toolbar/mod.rs:184-196` | save_pref 从管道线程与工具条线程并发写同一 tmp 文件 | tmp 名加线程 id，或收敛到单线程写 |

### panic 面 / guard

| # | 位置 | 问题 | 修法 |
|---|---|---|---|
| G1 | `langbar.rs:13` 承诺 vs 实现 | 「全部 COM 回调经 guard 包装」失实：langbar/ui_element/两个 DoEditSession/OnCompositionTerminated/wndproc 全部裸奔（当前恰好无可达 panic 源，后续改动极易破防） | 补 guard（沿用现有 helper），或修正注释为如实范围——推荐补齐 |
| G2 | `iuv-daemon/main.rs:263` | `_ => unreachable!` 对 Request 枚举演进脆弱（同 36-P1） | 改 `Response::Err { msg }` |
| G3 | `iuv-tsf/composition.rs` 两侧 + `iuv-daemon/toolbar/mod.rs:247-259` | wndproc/消息循环线程无 catch_unwind，panic 即整进程 abort | DispatchMessage 外包 catch_unwind(AssertUnwindSafe)，与其余线程纪律对齐 |
| G4 | `iuv-data/opencc.rs:60-68` | count 无上界即 with_capacity，12 字节损坏文件 → 分配器 abort（非可降级 Err） | 仿 dict.rs:175 先用文件大小钳制（count × 最小记录长 ≤ len）再分配 |

### 平台兼容

| # | 位置 | 问题 | 修法 |
|---|---|---|---|
| W1 | `iuv-win/popup.rs:137-140` | client_pos 零扩展而非符号扩展——多显示器负坐标区（左/上方副屏）悬停/点击全错 | `(v & 0xFFFF) as u16 as i16 as i32`（GET_X_LPARAM 语义） |
| W2 | `registration.rs:144-200` | 注册 DISPLAYATTRIBUTEPROVIDER 但未实现接口（查询方 E_NOINTERFACE） | 移除该类别注册或实现接口（M3 决策） |
| W3 | `text_service.rs:432-433` | Deactivate 不终止 composition，预编辑以普通文本残留，与 Ctrl+Space flush 语义不一致 | **决策项**：确认是否有意（切输入法残留 vs 上屏），定后补注释或对齐 |

### 性能（按键热路径）

| # | 位置 | 问题 | 修法 |
|---|---|---|---|
| F1 | `iuv-tsf/log.rs:46-63` | 每条日志一次 CreateFile/CloseHandle + GetModuleFileNameW + 两次 env 查询；禁用模块仍付 format! 成本（注释「不构建」不成立）；daemon 离线每键 2 条失败日志刷屏 | pid/module_name/log_path OnceLock 缓存；log_line 改取闭包或宏先判模块再格式化；离线日志只在 set_online 翻转处记 |
| F2 | `iuv-core/session.rs:404` | effect() 每键对全部候选 convert_candidate（双 String 克隆 + 简繁查表），简体模式白付 | script==Simplified 时整体跳过转换分支（直接 clone） |
| F3 | `iuv-core/engine.rs:219` + `routes.rs:220` | 每键克隆整份 Config 两次（含 Vec 字段） | 锁作用域内只拷出所需标量（candidate_prefix/max_candidates），不克隆整份 |
| F4 | `iuv-data/userdict.rs:233` | is_blocked 每候选分配 2 个 String，merged retain 每渲染页 2×N 次堆分配 | block 表改 `BTreeMap<String, BTreeSet<String>>`，`get(code).contains(word)` 零分配借用查找 |
| F5 | `iuv-ui/render.rs:65/122`、`text.rs` 共享 Buffer | 标签每键格式化两次未复用 sizes[i].0；measure/draw 交替致整窗标签双重整形 | 绘制阶段复用测量标签；文档注明 measure/draw 相邻调用代价（双 Buffer 留 M9 皮肤时再做） |
| F6 | `iuv-daemon/settings.rs:365,472-482` | 词库标签页每帧持锁全量格式化用户库——UI 卡顿连带管道延迟尖刺 | 快照缓存 + 版本变化时刷新，或分页 |

### 数据 / API 正确性

| # | 位置 | 问题 | 修法 |
|---|---|---|---|
| A1 | `iuv-ui/text.rs:130-132` | cosmic-text Color 是 0xAABBGGRR，代码按 RGBA 提取 → **红蓝互换**，当前灰阶调色板掩盖，彩色文字/皮肤落地即爆 | 交换 r/b 两行提取 |
| A2 | `iuv-data/mmap.rs:53` | FILE_SHARE_WRITE 允许他进程就地截断文件 → 视图读越 EOF 页 AV（当前 rename 部署流打不到，属未防御假设） | 去掉 WRITE 共享位（只留 READ|DELETE），或注释固化「禁止就地改写」契约 |
| A3 | `iuv-data/Cargo.toml:10` | windows-core 无条件依赖，仅 cfg(windows) mmap 用一处 PCWSTR | 移入 `[target.'cfg(windows)'.dependencies]` |

---

## 三、P2 打磨（择机，多为低危/风格）

- **IPC 安全债显式化**：默认 DACL + 固定管道名 + 无对端校验（抢注面）+ 共享段同用户可写——单用户
  桌面可接受，**记入 M7 安装器里程碑**做限制性 DACL / 随机后缀名。
- `iuv-win/lib.rs:31-48` transmute(usize→fn)：改 AtomicPtr<()> 收窄；补「logger 不得递归/panic」契约注释。
- `ulw.rs:35-37` unsafe impl Send 与自身注释矛盾：改受约束封装或修正注释。
- `popup.rs:146` get_self::<T> 与窗口类无绑定：类名常量与 T 关联（trait WindowClass）；补 hCursor/CS_DBLCLKS。
- `ui_element.rs:293-313` GetString 越界白克隆一次；`:337-341` upagecnt=0 理论 OOB 读。
- `langbar.rs:156-166` 非 VT_I4 VARIANT 应 VariantClear（当前不可达）。
- `codec.rs:42-47` ERROR_MORE_DATA 识别 + 编码端帧长断言；~~ToolbarState 解码值域校验~~（**已完成 2026-08-21**：四态/布尔字节解码即校验 0/1，非法整条拒绝）。
- `config.rs:197-201`（daemon）保存无 fsync，对齐 UserDict::save 的 sync_all 做法。
- 重复收敛（并入 36 号批次）：scale 净化 ×4、路径解析 ×5（36-D6）、pipe connect 双份（36-D7）、
  render.rs:182-185 与既有常量重复、settings theme/orientation 裸 String 改枚举。
- `iuv-repl/main.rs:97-106` 无会话静默吞键加提示。
- `engine.rs:261` is_syllable_prefix 线性扫 407 音节 ×每键多次（量小，可做前缀索引，同 36-四）。
- MapVirtualKeyW 每键最多 3 次独立求值（key_routing.rs:72/78/83）合并一次。

## 四、明确不做 / 维持现状

- mmap 截断 AV 的运行时防御（部署流走 rename，A2 去共享位即可）；
- FontSystem 跨窗口共享 Database（仅两窗口，收益小）；
- 用户库写时复制换持久化结构（几百条规模下整表克隆可接受，词条上万再议）;
- 自定义错误类型/thiserror（io::Error + 上下文消息对本规模合适）。

## 五、执行批次与验证

原则：每批独立可验证、全程 `cargo test --workspace` 兜底、TSF 侧改动以 dev-deploy + 手测收尾。

| 批次 | 内容 | 预估 |
|---|---|---|
| 批 1（P0） | R1-R6 六项 + 各自回归测试 | 半天 |
| 批 2（panic/guard） | G1-G4 + W1 | 半天 |
| 批 3（并发） | C1-C6（C3 工作量最大，可单独成批） | 1 天 |
| 批 4（性能） | F1-F6（F1/F3/F4 优先） | 半天 |
| 批 5（数据/API） | A1-A3 + P2 择项 | 半天 |

批次间无强依赖（C3 依赖 C2 的 Drop 语义理清，建议 C2→C3 顺序）。批 1 完成后红线清零，
可随时停在任何批次。
