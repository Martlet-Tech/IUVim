# 53 单键延迟定位：`cursor_longer` 全量物化（on_key 20–80ms 的引擎侧根因）

## 现象

打字有明显卡顿。线上 `iuv-server` 的 `[perf]` 慢键观测（阈值 on_key ≥10ms）显示单键
`on_key` 20–80ms、偶发 100–280ms；关掉高频日志后尖峰消失，但底噪仍有 20–80ms。

## 定位方法

1. **纯引擎基准**（排除 IPC/序列化/候选窗）：`crates/iuv-core/examples/onkey_bench.rs`
   ——真实 125 万词库、release 构建、直接驱动 `Session::on_key`。
2. **分段探针**：`IUV_PROBE=1` 时在 `RimeEngine::translate`、`collect_buckets`、
   `Dict::cursor_longer` 打印各阶段耗时（临时脚手架，定位后已移除）。

复现：

```
CARGO_TARGET_DIR=target-server cargo run --release -p iuv-core \
    --example onkey_bench -- "%LOCALAPPDATA%\iuv\iuv.imedic" 3
```

## 数据

**纯引擎每键耗时**（无 IPC）：均值 3–8ms，峰值 30ms。已经偏慢（目标 <1ms），但明显
小于线上的 20–80ms——差额在服务端后半段（见文末）。

**`translate` 分段**（129 样本）：`buckets`（`collect_buckets`）占 **97%**
（平均 6.77ms / 总 6.99ms），`seg`/`graph`/`cands`/`poet`/`dedup` 均在微秒级。

**`collect_buckets` 分段**：物化阶段 `mat` 主导（`bfs` 0.1–8ms、`sort` ~30µs）；
`mat` 内部 **`dict`（取词条）主导**，`merge_into` 仅 0.1–1.2ms。

**`dict` 按调用类型**（典型慢样本）：

| 调用 | 次数 | 结果条数 | 耗时 |
|---|---|---|---|
| `cursor_longer`（补全边物化） | 2–4 | ~130 | **10–24ms** |
| `cursor_exact`（精确物化） | 522 | 15625 | 1.68ms |

`cursor_longer` 的**每条保留条目成本约为 `cursor_exact` 的 100 倍**。

**`cursor_longer` 扫描区间**（直接埋点）：

```
range=104453 kept=104453 dt=14.76ms
range=107552 kept=107552 dt=14.77ms
range=116655 kept=116655 dt=15.95ms
```

单次调用扫描并物化 **~10 万条记录**，最终只保留 `limit`（64）条。

## 根因

`crates/iuv-data/src/dict.rs::Dict::cursor_longer`：

```rust
let mut out = Vec::new();
for i in c.lo..c.hi {
    let e = self.entry_at(self.index_off(i));   // 每条记录都物化成 Entry（2 个 String 分配）
    if e.code.len() != c.plen { out.push(e); }
}
// …merged / select_nth_unstable_by / truncate(limit) / sort…
```

对**短前缀**（简拼/补全边的累计前缀可能只有 1 个字母，如 `y`），游标区间覆盖词典里
所有以该前缀开头的码——实测 10 万+ 条。函数把**整个区间全部物化**（10 万条 × 2 个
String ≈ 20 万次堆分配），随后 `select_nth_unstable_by` 选出 top-64、其余全部丢弃。

`Dict::prefix`（`:396`）是同一形状（bench 中未触发，但同样风险）。

## 修复方向

1. **只按权重扫、不物化**：先以零分配的「读 weight（u32）」遍历区间，选出 top-`limit`
   的下标，**只物化命中的 k 条**。10 万条权重扫描 ≪ 10 万条 Entry 物化。
2. **跳过等长码簇**：区间内码长 == `plen` 的记录必然被丢弃（`code.len() != plen` 才留），
   且因码序有序而**连续居首**——可二分定位边界，不物化它们（`cursor_exact` 已单独处理等长码）。
3. **保留既有顺序红线**：`merged`（用户库屏蔽/调权/独有条目）必须在截断**之前**生效
   （`prefix` 注释 §46 的教训），top-k 选择需与之一致。
4. 补全边的必要性本身可评估：能否在 BFS 阶段就用更便宜的条件剪掉注定进不了桶首的补全路径。

## 服务端分段实测（2026-10-12，重启后）

在 `iuv-server/src/lib.rs` 的 `Key` 分支按 engine / outcome / respond(IPC) / candwin
四段埋点（`[perf]` tag，阈值 5ms）；客户端在 `remote_host.rs` 记按键往返。实测：

| 阶段 | 平均 | 说明 |
|---|---|---|
| **engine**（`Session::on_key`） | **41.0ms** | 占 on_key 的 **99.9%** |
| outcome | ~10µs | 瘦身映射 |
| **respond**（IPC 管道写） | **0.000ms**（200–400ns） | 无背压 |
| candwin | 41µs | 无界通道，send 不阻塞 |

- 357 样本：`engine` 峰值 372ms；总耗时分布 <10ms:47 / 10–20:29 / 20–50:192 / 50–100:80 / >100:9。
- 客户端往返与 `on_key` 同毫秒配对 67 对：**Δ 平均 0.34ms**（0.11–3.47ms）——IPC 双程可忽略。

**结论：延迟 100% 在引擎，`outcome`/`respond`/`candwin` 全部无辜。**

## 用户词库放大效应（2.5–3×）

裸基准（不挂用户库）engine 3–8ms；线上 20–50ms。差异来自 `Dict::merged`：真实
server 装配了 `iuv.user.imedic`，`merged` 的屏蔽/调权分支随之激活——而它作用在
`cursor_longer` 物化出的**全量 ~10 万条**上（每条一次 `is_blocked` + `adjusted`
查找，随后整段 `sort_by`），再截断到 64。

基准挂上用户库后复现（`onkey_bench`）：

| 输入 | 无用户库 均值/峰值 | 有用户库 均值/峰值 |
|---|---|---|
| nihao | 2.99 / 14.10ms | **9.21 / 56.36ms** |
| jintiantianqizhenbucuo | 7.71 / 30.65ms | **16.65 / 87.74ms** |
| woshiyigechengxuyuan | 7.89 / 25.55ms | **21.97 / 87.52ms** |
| wangshangliulanqi | 7.54 / 29.70ms | **19.82 / 95.26ms** |

→ 与线上同量级，机制确认。修复必须**从源头避免全量物化**，而不是只优化 `merged`
（顺序红线：`merged` 必须在截断之前）。

## 备注

- 本次定位用的探针为临时脚手架，已全部移除；`settings.rs` 的 `LOG_MODULES` 补齐（同日
  的小修）保留。
- 基准程序 `crates/iuv-core/examples/onkey_bench.rs` 保留，供修复后复测。
