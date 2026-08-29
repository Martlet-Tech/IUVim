# 默认桌宠素材许可记录（M1 桌宠骨架 · 必填版权合规档案）

> 记录日期：2026-08-29 · 范围：仅 M1 内置默认宠（`assets/pet/default.png`）。
> 商用与修改授权以本文件记录为准；M2 起工坊 mod 走 Steam UGC 条款，许可由上传者担责。

---

## 1. 素材基本信息

| 项 | 值 |
|----|----|
| **资产名** | Dog Spritesheets（白色款 / spritesheet_white_0.png） |
| **作者** | Jason of GDN（OpenGameArt 用户名） |
| **来源 URL** | <https://opengameart.org/content/dog-spritesheets> |
| **直接文件 URL** | <https://opengameart.org/sites/default/files/spritesheet_white_0.png> |
| **本地落地** | `assets/pet/default.png`（与 `assets/` 顶层并列） |
| **M1 默认使用** | 仅 `spritesheet_white_0.png`（白狗，最广适配浅深双主题）；黑色 / 棕色文件未采用，可作未来换肤备选 |

## 2. 许可证全称

**Creative Commons Zero v1.0 Universal（CC0 1.0）— Public Domain Dedication**

OpenGameArt 页面（来源 URL）License(s) 字段明确标注 **CC0**。CC0 全文见：
<https://creativecommons.org/publicdomain/zero/1.0/>

CC0 摘要（不替代全文）：
> 在适用法律允许的最大范围内，创作者已完全放弃对作品的所有版权及相关权利，
> 将作品奉献至公有领域。允许任意使用、修改、再分发、商用，**无需署名**，
> 无任何 copyleft 传染条款，无 copyleft / share-alike 约束。

## 3. 商用 / 修改授权

| 授权项 | 是否允许 | 依据 |
|--------|----------|------|
| 嵌入闭源商业软件（iuv-daemon 闭源上 Steam） | ✅ 允许 | CC0 §3 — 任意使用，包括商业 |
| 修改 / 衍生（换色、加键盘姿势） | ✅ 允许 | CC0 §3 — 任意演绎 |
| 再分发（原始或修改版） | ✅ 允许 | CC0 §3 — 任意再分发 |
| 必须署名 | ❌ 不强制 | CC0 §2 — 放弃所有署名权（自愿致谢可保留） |
| Copyleft 传染 | ❌ 无 | CC0 为彻底放弃，无 SA / 衍生同等条款 |

## 4. M1 帧表布局约定（与 `crates/iuv-ui/src/pet.rs` / `platforms/windows/iuv-daemon/src/pet_assets.rs` 常量对应）

默认宠帧表规格（**实测**：读取 PNG IHDR 头）：

| 项 | 值 |
|----|----|
| 帧表尺寸 | 96 × 80 像素 |
| 单帧尺寸 | 16 × 16 像素 |
| 排列 | 6 列 × 5 行（行优先切割：`row * cols + col`） |
| 帧总数 | 30 帧 |
| 文件大小 | ≈ 2.2 KB（远低于 M1 上限 256 KB） |
| 颜色格式 | PNG 8-bit RGBA（透明背景） |

帧索引映射（行号 = 动画语义；素材缺哪个动作 → `PetSprites::frame()` 自动回退 Idle，模型与素材解耦）：

| 行（0-based） | 动画语义 | 帧数 | 用途（与 `PetModel` 配合） |
|---------------|----------|------|----------------------------|
| 0 | idle 待机 | 6 | `Idle`（闲置循环）；`ModeEn` 回退 Idle |
| 1 | walk 行走 | 6 | `Typing`（打字律动） |
| 2 | run 奔跑 | 6 | 预留（当前未映射 clip，保留作未来扩展） |
| 3 | jump 起跳 | 6 | `React`（点击互动跳一下）+ `Width`/`Script`/`Punct`（四态一闪） |
| 4 | attack 攻击 | 6 | 预留（当前未映射 clip，保留作未来扩展） |

> 注：原作者页面描述素材含 idle / walk / run / jump / fall / attack 等多组动作；
> 本帧表实际为 5 行 × 6 帧的统一网格，M1 锁定上述 5 行映射，多余行保留作 M2 扩展位。
> 行间不存在"左/右"区分（素材本身单朝向），无需做镜像。

## 5. 红线与未来动作

- **M1 范围内**：仅本文件列出的资产；任何新增默认宠 / 备选贴图必须在本文件追加对应
  段（来源 URL / 作者 / 许可全称 / 商用修改授权），禁止"裸跑"（沿用其他 mod / 同人包素材）。
- **M2 mod 工坊**：UGC 条款 + 举报机制；上传者担责（Steam UGC 条款），不在本档案覆盖。
- **替换默认宠**（如官方画师新做 32×32 桌宠套图）：在 `assets/pet/LICENSE.md` 追加新段、
  `pet_assets.rs` 更新内嵌常量、`assets/pet/default.png` 替换文件；不得修改本节既有 CC0 段落。
- **禁止来源**：Shimeji 社区同人包、未经原作者明确授权的二次创作素材（许可不明 / 可能侵权）。

---

## 6. 少女默认皮肤素材（`assets/pet/girl_default/`）

> 追加日期：2026-08-29 · 少女形象取代 M1 像素狗成为内置默认皮肤。

| 项 | 值 |
|----|----|
| **资产名** | `girl_default`（二次元少女半身像） |
| **来源** | AI 生成（腾讯混元 / Hunyuan 图像生成服务） |
| **生成提示词** | 见 `assets/pet/girl_default/SOURCES.md` |
| **本地落地** | `assets/pet/girl_default/`（含 `skin.json` 皮肤描述） |
| **处理工具** | `rembg`（背景移除，u2net 模型）、Pillow（几何切分与合成） |

### 6.1 商用与版权说明

| 授权项 | 状态 |
|--------|------|
| 作为 iuvim 内置默认皮肤使用 | ✅ 当前状态 |
| 商用分发 | ⚠️ **需自行评估** |
| 署名要求 | 无明确要求 |
| Copyleft 传染 | 无 |

**风险提示**：本素材由 AI 图像模型生成，其**可版权性与商用授权范围因司法管辖区和服务条款而异**，
目前尚无统一的司法结论。iuvim 项目将其作为内置默认皮肤使用；**若用于商业分发（如 Steam 上架），
请根据当地法律与生成服务条款自行评估风险**，必要时替换为有明确授权的人工绘制素材。
项目不对 AI 生成内容可能涉及的第三方权利主张承担责任。

### 6.2 M1 像素狗素材的保留

`assets/pet/default.png`（CC0 像素狗，见上文 §1~§4）**继续保留**，作为 L0 降级回退素材与对比基准，
不作为当前默认皮肤。上文 §1~§4 的 CC0 记录保持原样，不得修改。
