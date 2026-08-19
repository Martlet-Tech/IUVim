# 12 · 任务书 C：iuv-repl（CLI 调试前端）

> 属主文件：`crates/iuv-repl/**`
> 前置阅读：`00-overview.md`、`01-contract.md` §4（Effect/Session 行为）、`30-conventions.md`
> 依赖 `iuv-core` 的公共 API 与 `iuv-data::load`。纯 std，禁止加第三方依赖。

## 1. 目标

不注册输入法就能在终端验证引擎：加载编译产物 `iuv.imedic`，交互式输入拼音、看候选、翻页、选词。
同时提供 `--batch` 非交互模式，供组装手册做冒烟断言。

## 2. 交付清单

`crates/iuv-repl/src/main.rs` 单文件即可。

## 3. 行为规格

### 3.1 启动

```
iuv-repl <dict.imedic>            # 交互模式
iuv-repl <dict.imedic> --batch <拼音串>   # 批处理：打印该输入的整表候选后退出
```

- 词典加载失败 → stderr 明确报错，退出码 1
- `Engine::new(dict, Config::default())` 建引擎

### 3.2 交互模式

提示符 `>`，逐行读取：

| 输入 | 动作 |
|---|---|
| 字母/`'` 串（如 `nihao`） | **新建** Session，逐字符喂 `Key::Char`，打印最终 Effect |
| 空行 | 对当前会话发 `Space`（提交首选/原文） |
| `1`..`9` | 发 `Digit(n)` |
| `,` / `.` | 上翻页 / 下翻页（走 `apply_keymap` 与运行时一致，默认表 8f479f9） |
| `!` | `Esc` |
| `q` | 退出 |

打印格式（每次按键后刷新）：

```
ni'hao
 1.你好 2.泥嚎 3.尼好 ...        # 当前页，高亮项前无空格前缀或加 *，自选清晰即可
 [page 1/3 · total 120]
< committed: 你好                # end=Commit 时打印，会话丢弃
< cancelled                      # end=Cancel 时打印
```

### 3.3 批处理模式（组装冒烟用）

`iuv-repl data\iuv.imedic --batch nihao`：打印 reading 行 + 全表候选（每行 `序号<TAB>text<TAB>kind<TAB>weight`），退出码 0。

## 4. 测试

逻辑薄，不强制单测；DoD 用真实词典手动冒烟（属 W2）。
需包含一个 `#[test]` 级冒烟：用 `Dict::from_entries` 小词典走一遍"输入→空格提交"流程断言 committed 文本
（把核心流程抽成 `fn run_script(engine, keys) -> Vec<Effect>` 之类的可测函数即可，main 只做 IO 壳）。

## 5. DoD

```
cargo test -p iuv-repl
cargo check -p iuv-repl         # 无 warning
cargo run -p iuv-repl -- <真实.imedic> --batch nihao    # 打印正常（W2 验证）
```

## 6. 子智能体启动提示词

```
你负责实现 iuv 输入法 MVP 的 iuv-repl 模块（CLI 调试前端）。
先读 D:\Projects\vaim\docs\plan\00-overview.md、01-contract.md、30-conventions.md，
再读任务书 D:\Projects\vaim\docs\plan\12-mod-iuv-repl.md 并严格执行。
只能创建/修改 crates/iuv-repl/ 下的文件；只用 iuv-core/iuv-data 的公共 API；除 workspace 已声明依赖外禁止新增第三方 crate。
完成后必须满足 DoD：cargo test -p iuv-repl、cargo check -p iuv-repl 无 warning。
真实词典冒烟由主智能体组装时执行，你只需保证 --batch 模式逻辑正确。
最终回复：改动文件清单 + 测试输出摘要 + 任何偏离契约之处（应为无）。
```
