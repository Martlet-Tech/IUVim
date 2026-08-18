# 24 üe 输入形归一（lue/nue → lve/nve）

## 问题

主流输入法输入 `gonglue` 即出候选「攻略」；本输入法必须严格 `gonglve`。
`lve`/`nue` 之外，`lue`/`nue` 是通用输入形，本输入法不支持。

## 根因（三个层次叠加）

1. **源数据层**：rime-ice 用 v=ü 约定，攻略 键 = `gong lve`；词库无任何 `lue/nue` 键。
2. **音节表层**：`Dict::syllables()`（编译期收集）与 `dict.rs` 静态 SYLLABLES 只有 `lv/nv/lve/nve`，
   无 `lue/nue` → `gonglue` 被切为 `[gong,lu,e]`，永远组不出 gong+lüe 路径。
3. **查询键层**：即便切出 `lue`，词库键是 `gonglve`，exact 不命中。

即：全管线缺少 üe 的 u/v 输入形等价层。

## 语言依据（法）

- **《汉语拼音方案》（1958）** 韵母表注释：j/q/x/y 后 ü 去点写 u（jue/que/xue/yue），
  **ve 形非法**（jv/yv 不支持是正确行为，勿加）；l/n 后保留两点（lüe/nüe），
  lu/nu 为真实对立音节，**lv≠lu、nv≠nu 保持现状**。
- **《汉语拼音方案的通用键盘表示规范》（GB）**：v 为 ü 的通用键盘替代键 → v 形有明文。
- **lue/nue**：非正字法，但普通话无标准韵母 `ue`，无歧义 → 全行业 IME 通行。
  结论：**v 形为规范键（词库本来就是，且 GB 有据），ue 形作为输入别名归一到 v 形**。

## 改造方案

只加两个输入别名 `lue→lve`、`nue→nve`，其余一切不动（数据零改动）。

### 1. `crates/iuv-data/src/format.rs`（编译期）

`write()` 收集音节集后，显式注入 `"lue"/"nue"`：
```rust
// üe 韵母的去点输入形（l/n 侧别名；j/q/x/y 侧 jue/que/xue/yue 已在表中）。
// 非标准音节，只作输入识别；运行时 Quanpin 靠它切出 lüe/nüe 路径。
syllable_set.insert("lue".to_string());
syllable_set.insert("nue".to_string());
```
效果：`Dict::syllables()` 含 lue/nue → Quanpin 可切 `gonglue`。旧词库（无 lue/nue）加载不受影响。
不加进 `dict.rs` 静态 SYLLABLES（那是"标准音节表"，语义上不该收非标准输入形）。

### 2. `crates/iuv-core/src/schema.rs`（单点归一）

`Quanpin::backtrack` 匹配到 `"lue"/"nue"` 时，输出规范形 `"lve"/"nve"`：
```rust
if self.syllables.contains(&s[pos..pos + len]) {
    matched = true;
    let syl = &s[pos..pos + len];
    // üe 去点输入形 → 词库规范形（v=ü，GB《通用键盘表示规范》）
    let canon = match *syl {
        "lue" => "lve",
        "nue" => "nve",
        _ => syl,
    };
    cur.push(canon.to_string());
    ...
}
```
**这是唯一归一单点**：seg / plans / viterbi 键 / dict 查询 / 用户词典 code 全部规范形 `gonglve`，
M2 调权/屏蔽/自造词不因 u/v 两种输入分裂（code 恒为词库 v 形）。

### 3. 重编译词库 + 部署

`data/iuv.imedic` 需含 lue/nue 音节（元数据段）：删除后重跑 `scripts\install.ps1`（缺词库自动 dictc）
或手动 `cargo run -p iuv-data --bin dictc`，再 `scripts\dev-deploy.ps1`。

## 测试

- **schema**（schema.rs 单测）：`segment("gonglue")→[["gong","lve"]]`、`"lue"→[["lve"]]`、
  `"nue"→[["nve"]]`；回归 `"gonglve"→[["gong","lve"]]`、`"jue"/"que"/"xue"/"yue"/"lve"/"nve"` 不受影响。
- **data**（format/dict 单测）：`Dict::from_entries` 后 `syllables()` 含 `lue/nue`。
- **集成**（engine_session.rs）：fixture 加 `("gonglve","攻略",9279)`，输入 `gonglue` →
  候选含「攻略」、seg_len=2 整词上屏；回归 `gonglve` 同出攻略；`lu` 仍路非律、`jv` 无候选。

## 已知副作用（可选 polish）

输入 `gonglue` 时预编辑显示规范形 `gong'lve`（u→v，与主流显示 lüe 的"标准化"方向一致）。
可选 polish：`session.rs::candidate_preview` rule 4 比较前对 `code_plain` 与 `plain` 做同一归一，
使 u/v 两种输入形显示一致为 `gonglve`（无撇号、消除 gonglue→"gong'lve" 与 gonglve→"gonglve" 的差异）。