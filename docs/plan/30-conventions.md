# 30 · 全局约定（所有智能体遵守）

## 1. 工具链与风格

- Rust stable（MSVC toolchain），edition 2021，`rust-version = 1.85`
- `cargo fmt` 默认配置；提交前自检
- 依赖白名单见契约 §2；新增第三方 crate 必须先报主智能体批准（防止并行开发依赖漂移）
- 注释与文档用中文；标识符用英文；公开 API 写 `///` 文档注释

## 2. 错误处理

- `ime-data`：`io::Result`；格式错误用 `io::ErrorKind::InvalidData`，消息带文件名/偏移
- `ime-core`：引擎运行期**不返回错误**（查不到 = 空结果，永不 panic）；构造期错误由 `Dict` 加载方处理
- `ime-tsf`：DLL 内**绝不 panic 传播到宿主进程**——COM 边界捕获一切，记日志后降级（放行按键/隐藏窗口）；
  unsafe 块必须带 `// SAFETY:` 注释
- `ime-repl`：`Result<(), Box<dyn Error>>` 从 main 返回即可

## 3. 日志

- 仅 ime-tsf 有运行期日志：`%TEMP%\input-ime-tsf.log`（`log.rs` 提供 `log_line`，std 实现，不加日志框架）
- 其他 crate 不打日志；测试用 `assert` 说话

## 4. 测试纪律

- 每个 crate：`cargo test -p <crate>` 全绿 + `cargo check -p <crate>` 无 warning 才算完成
- 时间相关逻辑（M2 衰减）**必须**以参数注入 `now`，禁止测试用"此刻"充数——
  反面教材：WindInput 32 项衰减测试全用 `now == last_used`（decay 恰为 1.0）全绿，真机即挂
- 断言"某事没发生"的用例，必须配一条"相关机制确实在工作"的正向用例（防空假绿）
- 引擎测试一律 `Dict::from_entries` 小词典，禁止读真实词库文件（慢且脆）

## 5. 并行开发纪律

- 只改属主矩阵（契约 §6）内文件；要动契约 → 报告主智能体裁决，禁止私改
- `todo!()` 桩可以调用（视为他人地盘），不可以实现
- 发现别的模块 bug：记录并报告，不越权修复

## 6. 词库合规

- 白霜拼音 rime-frost = **GPL-3.0**：数据由 `scripts/download-dict.ps1` 下载到 `data/`（gitignore），
  不进仓库、不编进二进制；发布包若含编译产物需在 NOTICE 声明
- M3 万象语言模型（CC-BY-4.0）需署名；届时再议

## 7. 构建/测试/注册速查

```powershell
cargo check --workspace                 # 骨架/日常检查
cargo test --workspace                  # 全量测试
cargo build -p ime-tsf --release        # 产出 DLL
scripts\download-dict.ps1               # 下词库
cargo run -p ime-data --bin dictc -- ...# 编译词库（见 20-assembly §3）
scripts\register.ps1                    # 注册（管理员）
scripts\unregister.ps1                  # 注销（管理员）
```
