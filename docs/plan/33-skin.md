# 33 · M9 可自定义贴图皮肤框架（IUVSKIN01）— 调研与可行性（仅分析，未实现）

> 状态：**调研定稿 / 挂起**（2026-08-20 决策：**只做可行性分析与生态调研，不写代码**）。
> 前置：M8 悬浮工具栏（32-status-toolbar.md，feat-toolbar 分支，2026-08-19 效果差、用户暂缓改进）完成后才动工。
> 定位：候选窗 UI 的"换肤"能力——用户放皮肤目录（manifest + PNG）即换皮肤，无需改代码。
> 本文记录：①librime/librime-lua 生态调研（本方向为什么值得做、合规结论）；②皮肤框架技术可行性（架构/格式/改动清单/风险）。

## 1. 缘起：为什么从"Lua 插件兼容"转向"贴图皮肤"

### 1.1 调研结论（2026-08-20，GitHub 实测）

原规划 `23-m8-plugin.md` 预留 `runtime:"lua"` 字段 = 兼容 Rime 的 Lua 插件生态。调研后确认该生态**极小众**：

- **librime 核心不内置 Lua**：Lua 能力来自外部插件 `hchunhui/librime-lua`（BSD-3-Clause，`Copyright (c) 2021`）。Windows/Weasel 上它靠**替换 `rime.dll`** 注入（Weasel 把 librime 静态编进自带 DLL），官方安装包默认**不带 Lua**。
- 全 GitHub 搜 `librime-lua` 仅 13 个仓库，其中**用户级 Lua 插件 ≈ 4 个**，合计不到 100 星：
  - `shewer/librime-lua-script`（62⭐，脚本合集，**无许可证**）
  - `unfiled0/librime-lua-water`（32⭐）
  - `codeMonkeyWang/librime-lua-stockprice`（1⭐）
  - `skykeyjoker/rime-cloud-pinyin-async`（1⭐，MIT）+ `xing133/rime-wenyun`（1⭐，MIT）——云拼音是当前仅存的活跃 Lua 场景
- Rime 生态主流扩展 = **YAML 方案 + 词库 + filter 配置**，不碰 Lua；Lua 是少数派折腾行为。

**决策**：不做 Lua VM 兼容。成本收益比上，**可自定义贴图皮肤框架**（主流输入法通用能力、用户可直接感知）远高于个位数星级的 Lua 插件兼容。

### 1.2 皮肤格式合规分析（2026-08-20 用户确认）

问：兼容主流输入法（搜狗 `.ssf` / QQ 皮肤）的皮肤文件格式，合法吗？

**结论：合法，红线一条。**
- 文件格式 = **互操作接口**，不是版权"表达"；独立实现解析器读取 zip 结构 + 图片资源 + 描述字段，属格式互操作（对照 ImageMagick/GIMP 读专有格式；《计算机软件保护条例》第 29 条认可）。
- 皮肤包位图是第三方作品，**仅运行时读取，不打包进发行物**，不涉及自身版权。
- **红线**：不抄搜狗/QQ 的解析代码（闭源格式，主流做法 = 逆向 + 独立重写）；不借对方名号背书。

**最终决策（用户拍板）**：**只做自研原生格式 `IUVSKIN01`**，不做搜狗/QQ 导入适配器（避免逆向维护成本 + 版权边界最干净）。将来若需导入兼容，按 §5 的适配器位加解析器即可，渲染层共用。

## 2. 目标（验收一句话）

用户放一个皮肤目录（`manifest.json` + 多张 PNG）到 `%LOCALAPPDATA%\iuv\skins\<name>\`，改配置即换候选窗皮肤：
候选窗背景 / 高亮行 / 悬停框 / 页码区可贴图（9-patch 缩放），文字色由配置覆盖。加载失败静默降级 light/dark，绝不崩。

## 3. 关键技术可行性（2026-08-20 验证）

### 3.1 零新增依赖（依赖白名单不变）

- **tiny-skia 0.12 默认启用 `png-format` feature**，自带 `Pixmap::decode_png` / `load_png`（8-bit RGB/RGBA/灰度，**索引色 PNG 不支持**——约束记入 §5）。
- **`draw_pixmap` 支持缩放绘制**：内部走 `Pattern` shader + `fill_rect`，传入非 identity `Transform` 即拉伸——**9-patch 边/中块缩放直接可实现**，无需第三方图像库。
- 结论：`Cargo.toml` 与契约 §2 白名单**零改动**。

### 3.2 架构落点

皮肤逻辑全部在 **iuv-ui**（跨平台纯 Rust），与 M4 渲染层同层：

```
config.json  skin: "my_skin"
   ↓
iuv-tsf text_service：load_skin(skins_dir, name) → 构造带 skin 的 Theme → candwin.set_theme（M6 热载通道复用）
   ↓
iuv-ui render：有 skin → NinePatch 画贴图背景 + 颜色覆盖；无 skin → 原矢量路径（light/dark 零变化）
```

### 3.3 皮肤目录格式（IUVSKIN01）

```
%LOCALAPPDATA%\iuv\skins\<name>\
├── manifest.json   # 名称/区域映射/9-patch 边距/颜色覆盖
├── bg.png          # 窗口背景（9-patch）
├── hl.png          # 高亮行背景（9-patch）
├── hover.png       # 悬停虚线框贴图（可选）
└── page.png        # 页码区域贴图（可选，缺省用色）
```

- `areas` 指定各区域 PNG 文件名 + 9-patch 边距 `{left,top,right,bottom}`；
- `colors` 覆盖 fg/hl_fg/page_fg（贴图不含文字）；
- **部分皮肤 = 部分贴图**：缺省区域回退 Theme 对应色，渐进增强；
- 与 plugins 同源"一目录制"（`skins/` 子目录名即身份）。

## 4. 改动清单（实现时的顺序，自上而下）

| # | 文件 | 改动 |
|---|---|---|
| 1 | `crates/iuv-ui/src/skin.rs` **新** | `Skin` 加载器（manifest 解析 + PNG 解码 → 区域 `Pixmap`）+ `NinePatch`（9 块切分，`draw_pixmap` + scale transform）+ 颜色覆盖。失败 → `None`。 |
| 2 | `crates/iuv-ui/src/theme.rs` | `Theme` 加 `skin: Option<Skin>`；light/dark 均 `None`，零行为变化。 |
| 3 | `crates/iuv-ui/src/render.rs` | `render_to_surface`：有 skin → 背景/高亮/悬停/页码走 NinePatch；无 skin → 原路径。文字（cosmic-text）不动。 |
| 4 | `crates/iuv-ui/src/lib.rs` | 导出 `skin` 模块。 |
| 5 | `crates/iuv-core/src/config/mod.rs` | 新增 `skin: Option<String>`（目录名，默认 `None`）+ 序列化/测试。 |
| 6 | `platforms/windows/iuv-tsf/src/com/text_service.rs` | 装配时 `load_skin` → 注入 candwin；`set_theme` 热载复用。 |
| 7 | `platforms/windows/iuv-daemon/src/settings.rs` | 外观页"皮肤"下拉（扫 `skins/` 子目录）+ 无皮肤选项。 |
| 8 | `scripts/install.ps1` / `dev-deploy.ps1` | 预创建 `skins/` 目录（空/示例皮肤）。 |
| 9 | 文档 | 契约 §2.2（零新依赖注明）、02-conventions §6（贴图皮肤合规）、AGENTS 状态区。 |

## 5. 约束与风险

- **PNG 限制**：tiny-skia 只支持 8-bit RGB/RGBA/灰度；**索引色 PNG 不支持**（manifest 文档注明）。
- **高分屏缩放**：9-patch 拉伸在 DPI 缩放下的绘制精度需真机验证（`draw_pixmap` scale transform 走 shader，视觉需抽查）。
- **热载**：皮肤目录改名/删除 → `load_skin` 返回 None → 降级 light，不崩。
- **皮肤资源版权**：`skins/` 由用户自放，iuv 不内置第三方位图；发行物不含皮肤资源（同词库 GPL 处理）。
- **合规红线**（见 §1.2）：不抄搜狗/QQ 解析代码；只做自研格式，不做导入适配器。

## 6. 验证（实现时）

- iuv-ui：skin 加载（合法/缺文件/坏 PNG/无边距）+ NinePatch 像素断言 + 部分贴图回退。
- iuv-core：config `skin` 默认 None / roundtrip / 未知值回退。
- `cargo test --workspace` + `cargo check -p iuv-ui` 无 warning。
- 手测：示例皮肤贴图渲染 / 9-patch 角不变 / 高亮行贴图 / 深色+贴图混合 / 缺贴图降级。

## 7. 挂起原因与重启条件

- **挂起**：前置 M8 悬浮工具栏（32-status-toolbar.md）效果差、需先改进（feat-toolbar 分支）。本任务书先行冻结调研结论，避免重复调研。
- **重启**：M8 悬浮工具栏达到用户可接受状态后，按 §4 清单实施。
