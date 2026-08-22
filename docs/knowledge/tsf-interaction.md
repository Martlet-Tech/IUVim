# TSF 关键交互点（机制规格）

> 定位：与 [`38-keyboard-flow.md`](38-keyboard-flow.md)（行为规格）配对的**机制规格**——
> iuv 与 TSF 打交道的每个关键接口：它本质是什么、我们怎么用、由它能自然理解哪些现象。
> 行为规格回答"用户按 X 会怎样"，本文件回答"这件事在 TSF 层是怎么发生的"。
> 源码映射总表见文末。

## 0. 全局图景：进程模型与两类应用

```
┌─ Word.exe 进程 ──────────────┐   ┌─ notepad.exe 进程 ───────────┐
│ Word(TSF-aware)              │   │ notepad                       │
│  ↕ 直接对话                  │   │  ↕ CUAS 系统桥（模拟对话）    │
│ iuv_tsf.dll（本进程实例）    │   │ iuv_tsf.dll（另一份实例）     │
└────────── itfmemgr ──────────┘   └───────────────────────────────┘
        ↑ 共享段只读引用 + ctl 管道（pid:tid）↑      daemon 统一持有用户库
```

- 输入法是 **cdylib，被注入每个用它的应用进程**——每个进程一份独立实例（各自的
  会话/composition/管道端点），跨进程共享的用户库走共享段只读引用 + daemon 写【现状】。
- `ITfThreadMgr` 是 TSF 的进程内总管理器，`Activate` 时发给我们一个 **client_id**——
  之后一切编辑请求都要报这个 id 证明身份【现状，text_service Activate】。
- **两类应用**：TSF-aware 应用自己实现 TextStore 接口与我们直接对话；非 aware 应用由
  系统的 **CUAS 桥**（Cicero Unaware Application Support）模拟出同样的对话面。两者的
  行为差异是历史上多个诡异 bug 的根源（Excel 编辑栏跳焦点、WoW 内 GetTextExt 不准）。
- **线程纪律**：TSF 对象基本绑定创建线程——我们的窗口必须建在使用线程、composition
  只能在其创建线程操作，均源于此【现状，toolbar/设置页独立消息泵的原因】。

## 1. 按键管线：TestKeyDown / KeyDown 吃键协议

```
用户按下 'd'
    │
    ▼
应用收到 WM_KEYDOWN(VK_D)
    │
    ▼
TSF-aware 应用调 ITfKeystrokeMgr::TestKeyDown
    │
    ▼
OnTestKeyDown ◄──「你要不要这个键？」（必须无副作用，纯判定）
    │
    ├── 吃 (S_OK) ─────► ITfKeystrokeMgr::KeyDown
    │                        │
    │                        ▼
    │                    OnKeyDown：真正处理
    │                    （进会话 / 改预编辑 / 提交……）
    │                        │
    │                        ▼
    │                    应用跳过自己的按键处理
    │                    （'d' 被输入法独占，不会进文档）
    │
    └── 不吃 (S_FALSE) ─► 应用继续 TranslateMessage
                             │
                             ▼
                         产生 WM_CHAR('d') → 字符直接进文档
```

- **本质**：两次询问协议——Test 问意图、KeyDown 执行。应用据此决定要不要自己处理该键。
- **铁律：Test 与 Down 必须共用同一判定函数**（我们的 `route_key`）。若 Test 说吃而
  Down 放，字母会被静默吞掉【现状注释，2026-08-19 Caps 直通实测教训】。
- **自动重复**：按住不放（含 Shift 等修饰键）会持续收到重复 KeyDown；修饰键本身不在
  `map_key` 白名单 → 一律放行，无副作用。
- **修饰键状态不靠事件**：组合判定用 `GetKeyState(VK_SHIFT)` 在处理当下实时查询
  （key_routing.rs）——重复风暴与应用转发差异都不影响正确性【现状设计】。
- **KeyUp 是空桩**（text_service.rs:526/540 永远放行）：没有任何逻辑依赖"抬起"时刻。
- **PreservedKey 是另一条预注册通道**（先注册组合键 → 命中直达 OnPreservedKey，
  连 composition 都不用开）：我们目前**零注册**、处理桩永远放行——给将来留的口子。

## 2. Edit Session 与 ec cookie——一切文档读写的锁

- **本质**：应用的文档不允许随手改。任何读写（取选区、改文字、量光标）都必须包在一次
  **编辑会话**里：`RequestEditSession(client_id, 会话对象, TF_ES_SYNC | TF_ES_READWRITE)`
  → TSF 回调我们的 `DoEditSession(ec)` → **ec（edit cookie）就是本次锁内操作的门票**，
  后续每个 SetText/GetSelection/SetSelection 都要出示它。
- **同步 vs 异步**：`TF_ES_SYNC` = 立刻执行（拿不到锁就失败，错误码
  `TF_E_SYNCHRONOUS`，应用正忙时预编辑更新会丢一轮）；小狼毫用 `TF_ES_ASYNCDONTCARE`
  排队等。我们选同步【现状，composition.rs::request】——延迟确定、实现简单，代价是
  忙时丢帧（可接受：下一键自愈）。
- **粒度**：一次 DoEditSession 里可以做任意多件事——我们的 commit 就是
  GetRange + SetText + Collapse + SetSelection + EndComposition 五连，全部持同一张票。
- **由此自然理解**：
  - 日志里 `do_edit_session: xxx 失败` 都是锁内调用失败（如 Code.exe 的 GetTextExt
    `0x80040206`——应用内部错误，我们记日志沿用旧光标降级）；
  - edit session 结束后 cookie 作废，所以每次按键都要重新 RequestEditSession。

## 3. ITfRange——活的书签对（不是文本拷贝）

```
文档字符流:   今|天|天|气|不|错|
                   ↑start       ↑end
                   └─── range ──┘
SetText("你好世界") 后锚点自动外扩:
              今|你|好|世|界|不|错|
                   ↑已罩住新内容↑
```

- **本质**：range = 文档流上**两个锚点之间的区间**。它不持有文本，是两个书签式的
  活引用——文档别处增删时 TSF 自动推移锚点，range 始终精确罩住当初圈住的内容。
- **我们的用法**（全部经 `comp.GetRange()` 取回当前组合的 range）：
  - `SetText(ec, 串)`：整体替换范围内文本，锚点自动伸缩跟随；
  - `Collapse(ec, TF_ANCHOR_END)`：两锚点并成末尾一个点；
  - 空串 SetText = 删除范围内容（cancel 语义）。
- **由此自然理解**：
  - Excel「的:」现象——应用往组合范围**外面**插的字面字符不属于 range，我们的
    SetText 动不到它；
  - 「全程单个 composition 只做全量 set_text」能成立，正因为 range 引用跨 edit
    session 持续有效。

## 4. ITfComposition——组合的生命周期

```
字母键 → StartComposition(ec, range, sink)   打上 GUID_PROP_COMPOSING 属性
       → 每键 GetRange + SetText             全量更新组合文本
       → SetText(最终串)+EndComposition       定稿：属性移除、所有权归应用
```

- **StartComposition 时必须给 sink**（组合销毁回调）：TSF 规范要求非空【现状，
  CompositionSink 仿 Weasel/SampleIME】。
- **EndComposition 之后 range 引用仍有效**但组合方法一律 E_UNEXPECTED——定稿只是
  摘属性，文字已是应用文档的普通部分。
- **OnCompositionTerminated 双重身份**：①应用强杀组合（Explorer 地址栏遇 `:`）；
  ②我们自己 EndComposition 也可能触发它（TSF 怪癖：空组合结束同样通知）。区分手段 =
  共享槽比对：通知的 composition 是否仍是当前持有的那个【现状，weasel 同款防御】。
  外部终止 → 清槽置标志 → 下一键丢弃会话降级重建【现状，Excel/Word 修复的基础设施】。
- **临时组合**：会话外的中文标点/原文直接上屏也走同一套原语——建一次性 Composition
  → set_text → commit → 即弃【现状，mode.rs::commit_punct】。

## 5. Selection——光标就是一个折叠到一点的选区

- **本质**：`TF_SELECTION = range + 样式`。应用文档的插入光标，就是一个**两端锚点
  重合的选区**。IME 说"把光标挪过去"= `SetSelection(折叠后的 range)`。
- **我们的用法**：
  - StartComposition 从当前 selection 拿初始范围【现状，仿 Weasel 用
    InsertTextAtSelection(QUERYONLY) 造合法 range】；
  - 每次预编辑更新后 `Collapse(END) + SetSelection`——让应用光标始终跟在组合文本尾；
  - **commit 定稿后同样补一次**——Word 类应用 EndComposition 不动光标，会把光标留在
    自己记录的组合起点【2026-08-22 修复：d72809e，光标落新文字前的根因】。
- **由此自然理解**：Excel「光标停在 : 之前」——应用插的字面冒号在组合范围外，我们把
  组合范围末端设为光标，自然落在冒号前。

## 6. Compartment——跨进程状态槽与 OPENCLOSE 真相源

- **本质**：TSF 提供的三级作用域（全局/线程/上下文）命名值槽（VARIANT），读写皆可，
  变化可订阅。它是 IME 与系统/语言栏之间**不经文档、不经按键**的状态通道。
- **我们的用法（中英切换）**：
  - `GUID_COMPARTMENT_KEYBOARD_OPENCLOSE`（该线程输入法的开/关态）作为**唯一真相源**：
    系统热键（Ctrl+Space 切换输入法/非输入法）由系统写入；语言栏点击归一为同款写入；
    我们订阅变化统一响应【现状，2026-08-12 定稿】；
  - Activate 时把 config `initial_state.mode` 写为激活初值【现状，28 号任务书】；
  - 日志 `OPENCLOSE 已有值 open=true（保持）` 即读取路径。
- **由此自然理解**：中英切换为什么"走系统机制"——因为切换热键本来就在系统手里，
  compartment 让我们与系统读写同一个值，而不是各自维护再打架。

## 7. 候选窗呈现：自绘窗口 + GetTextExt 定位

- **定位**：组合/光标的屏幕矩形来自 `ITfContextView::GetTextExt(ec, range)`——返回
  屏幕坐标包围盒 + clipped 标志；最小化/不可见时返回全零矩形【现状，caret 采集分支】。
  候选窗（自绘 ULW 分层窗口）贴此矩形下方弹出。
- **应用差异的现实**：GetTextExt 在不同应用质量参差——CUAS 桥下可能不准（Weasel 曾用
  零宽空格填充 workaround）；Code.exe 返回 `0x80040206` 内部错误【日志实测】。我们的
  策略：失败/clipped/全零 → 沿用旧光标或隐藏候选窗，绝不崩。
- **呈现通道两条**：
  - 主路：自绘候选窗（iuv-ui 渲染 + ULW 上屏），不经过 TSF；
  - 辅路：`ITfUIElementMgr`（日志 `[uielem] QI 成功`）——向应用提供标准候选 UI 元素；
    自绘应用名单（candidate_owner_apps，如 WoW）命中时不自绘、由游戏桥拉取候选数据
    【现状，28/32 号任务书】。
- **焦点切换不打断会话**：OnSetFocus 仅隐藏候选窗防悬浮他应用之上，session/composition
  原样保留【现状，2026-08-21 设计原则，Excel 首键修复的基础】。

## 8. 源码映射总表

| TSF 交互点 | 我们的位置 |
|---|---|
| OnTestKeyDown/OnKeyDown/KeyUp/PreservedKey 桩 | iuv-tsf `com/text_service.rs:520-550` |
| 路由判定（Test/Down 共用） | iuv-tsf `com/key_routing.rs::route_key` |
| 键映射白名单（含 OEM 符号收编） | iuv-tsf `session_bridge.rs::map_key` |
| EditSession 请求 + Range/Composition 操作 | iuv-tsf `composition.rs` |
| 组合销毁 sink（外部终止防御） | iuv-tsf `composition.rs::CompositionSink` |
| 会话外标点/原文直通上屏 | iuv-tsf `com/mode.rs::commit_punct` |
| 中英切换 compartment | iuv-tsf OPENCLOSE 读写 + OnChange 响应 |
| 候选窗定位（GetTextExt） | iuv-tsf `composition.rs` caret 采集 + `ui/candwin.rs` |
| UI 元素辅路 | iuv-tsf `ui_element.rs` |

## 变更记录

- 2026-08-22 初版：随 issue「d冒号表现不一致」修复过程中的机制问答沉淀成文
  （Range/光标/Esc/结束信号等口头讲解首次落档），与 38 号行为规格配对。
