# 53 · 单键延迟 20–80ms 治本（两轮：cursor_longer 全量物化 + poet-only 桶全簇物化）

> 状态：**已结（2026-10-12）**——两轮修复落地（提交 `9d60c43` 一阶 + `63b5ca4` 二阶），
> 真机回归通过：线上慢键（≥5ms）从 317/场 → 103/场 → **8/场**，长尾 227ms → 19ms 消失。

## 现象

打字有明显卡顿。线上 `iuv-server` 的 `[perf]` 慢键观测（阈值 on_key ≥5ms）显示单键
`on_key` 20–80ms、偶发 100–280ms；关掉高频日志后尖峰消失，但底噪仍有 20–80ms。
按键分布高度集中在短前缀码：z 均值 92.9ms > s 74.7 > y 69.2 > d 57.5 > m 31.3。

## 定位方法

1. **纯引擎基准**（排除 IPC/序列化/候选窗）：`crates/iuv-core/examples/onkey_bench.rs`
   ——真实 125 万词库（+ 用户库对齐装配）、release 构建、直接驱动 `Session::on_key`。
   首轮复现：长句单键均值 8–20ms、峰值 97ms，与线上吻合 → 延迟在引擎内。
2. **观测埋点**：server `[perf]` 慢键测量窗口拆 engine / outcome / respond(IPC) /
   candwin 四段（`iuv-server/src/lib.rs`）+ TSF 客户端「按键往返」观测（`remote_host.rs`）。
   四段数据显示 **engine 占比 99.8%**，IPC/候选窗洗清嫌疑。
3. **分相探针**（临时脚手架，定位后移除）：`translate()` 拆 seg / graph / buckets / post
   ——**buckets（collect_buckets）占 95–99%**；再细分物化分支——`longer`（补全标记的
   大簇扫描）与 `best`/`exact`（同音簇物化）分列热点。

## 根因

### 一阶（`9d60c43`）：`cursor_longer` / `prefix` 全量物化

`Dict::cursor_longer` 把游标区间**逐条物化成 Entry**（2 String/条）再 select top-64。
短前缀（单字母声母/音节前缀）的区间覆盖 10–20 万条 → 单次调用 20–40 万次堆分配。

**放大器（比初判更精确的一层）**：补全边的 `tail_start` 同时是上一段 Normal 边终点，
必在 `origins` 集合里；而 **origin walk 以 root 游标（key=""）起步**——它走补全边拼出的
键就是裸字母（`s`/`y`/`z`），`cursor_step(root, 单字母)` 得到**全表级单字母簇**。打字时
每敲一个新词的首字母必命中此状态（非段首 origin 走的补全边是父区间作用域，区间很窄，
无害——探针对照 `woshiy` y=62ms vs `woshi'y` y=1.7ms 的 30 倍差即来源于此）。

### 附带 bug（同笔修复）：补全边 `tail_start` chars/bytes 错位

`rime/mod.rs` 注入 2b 补全边时 `tail_start` 按 **chars** 累加、按 **bytes** 坐标使用：
输入带 `'` 时坐标错位到撇号顶点，边落在无 walk 可达的顶点上**静默失效**
（`woshi'y` 的 y 补全丢失）。旧「快」其实是候选语义丢了。

### 二阶（`63b5ca4`）：poet-only 桶全簇物化 + merged 逐条分配

一阶修复后探针显示 buckets 仍占 95–99%，细分：origin≠0 的标记（每段起点 ~25 个音节
简拼边）走 `cursor_exact` 全簇物化——但这些桶**只喂 Poet，且 Poet 每 (起,终) 只取桶首**
（`build_poet_graph` max_homophones=1），全簇物化纯属浪费。另 `merged()` 屏蔽过滤
逐条 `is_blocked`（每条目 2 次 String 分配）在全簇路径上放大。

## 修复

### 一阶（`9d60c43`，crates/iuv-data + iuv-core/rime/mod.rs）

- `Dict::cursor_longer` / `prefix` 改 **零物化 top-k**：新增 `entry_ref_at`（借用视图，
  无分配）+ `top_entries`（权重扫描选 top-limit 后只物化命中条目）；等长码簇
  （code == 前缀，必被过滤）二分定位跳过。**顺序红线保持**：用户屏蔽/覆盖在扫描期
  行内生效（覆盖权重参与选优），回归钉 `prefix_weight_desc_without_user_dict` 原样通过。
- `UserDict::block_pairs()` 借用视图（逐条 `is_blocked` 不可用于热路径）。
- 补全边 `tail_start` 改**字节坐标**（逐段在 lower 上步进、段后跳一个 `'`；
  段与 raw 失配时保守放弃注入）。

### 二阶（`63b5ca4`，crates/iuv-data + iuv-core/rime/translator.rs）

- `Dict::cursor_exact_best` / `cursor_longer_best`：origin≠0 标记只物化**有效权重最优
  单条**，桶首与「全量物化 + merged + 截 tied」逐字段一致（屏蔽/覆盖/独有条目行内
  生效，平序 word 升序同 worse_first；独有条目夺魁时直接构造，不调 merged 以守
  「单条」契约）→ **Poet 组句零变化**。
- `range_best` 快路径：纯权重扫描（只读记录头 code_len/word_len 定位 u32，不碰
  UTF-8 校验/覆盖表）取 max + 并列集（通常 1–3 条）再精裁；判定条件「覆盖最大值 <
  基础最大」排除覆盖/独有夺魁可能；病态（全零权重簇、并列 >64、并列全被屏蔽/覆盖）
  回退借用全解析慢路径。
- `merged()` 屏蔽过滤改 `block_pairs()` 借用比对；`UserDict::max_adjustment()`。

## 数据

**基准**（onkey_bench，真实词库 + 用户库，3 轮，均值/最大）：

| 输入串 | 最初 | 一阶后 | 二阶后 |
|---|---|---|---|
| jintiantianqizhenbucuo | 16.45 / 97.3ms | 5.60 / 15.6ms | **2.78 / 5.8ms** |
| woshiyigechengxuyuan | 20.25 / 83.9ms | 5.07 / 14.6ms | **1.34 / 5.4ms** |
| nihao | 7.59 / 37.1ms | 1.49 / 6.4ms | **0.44 / 1.0ms** |
| wangshangliulanqi | 16.43 / 76.6ms | 8.47 / 27.1ms | **4.69 / 17.0ms** |

**线上**（`%TEMP%\iuv-server.log` / `iuv-tsf.log`，≥5ms 慢键，WorkBuddy 同场景）：

| 部署版本 | 慢键样本/场 | 客户端往返 p50 | max |
|---|---|---|---|
| 旧二进制 | 317 | 33.1ms | 227.6ms |
| 一阶 | 103 | 10.0ms | 40.1ms |
| 二阶 | **8** | **9.4ms** | **18.9ms** |

服务端四段拆分全程 engine 占比 99.8%+，IPC 往返 ≈1ms（传输层健康）。

## 测试

- 新增回归钉：`cursor_longer_top_k_with_user_semantics`（权重降序截断/等长码过滤/
  覆盖救回/屏蔽补位）、`cursor_best_single_entry_head_semantics`（桶首五契约）；
  存量 `prefix_weight_desc_without_user_dict` 等全绿（26 套件 0 失败）。
- 真机：WorkBuddy/notepad/Weixin/Code 多应用回归，候选正确性无反馈异常。

## 遗留 / 备注

- `wangshangliulanqi` h@5 的 17ms 为冷启动缺页离群点（探针实测同类扫描 ~1.1ms），
  预热后消失，不处理。
- `iuv-win` `rtt_bench` 为存量计时敏感抖动（约 1/3 概率失败），与本次无关。
- 剩余 <5ms 底噪 = 每键全量重翻译的结构成本（整串 BFS 重走 + cursor_step 二分）；
  如需再压可评估增量翻译/跨键桶缓存（涉及 Arc 渗透 BucketEntry，未立项）。
- 基准程序 `crates/iuv-core/examples/onkey_bench.rs` 保留，供后续复测。
- 定位用临时探针已全部移除；`settings.rs` 的 `LOG_MODULES` 补齐与 `[perf]` 四段
  埋点保留为常驻观测。
