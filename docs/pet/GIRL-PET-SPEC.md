# 二次元少女桌宠规格（GIRL-PET-SPEC）

> 范围：M1 默认桌宠形象改造  
> 状态：已实现并落盘  
> 默认皮肤：`assets/pet/girl_default/`  
> 关联文档：`PRD.md`、`UIUX.md`、`assets/pet/LICENSE.md`  

---

## 1. 产品定位

把 iuvim 输入法浮动工具栏的桌宠从 40×40 像素狗挂件升级为**二次元少女半身像**。
少女趴在工具栏上沿（栖木式），具备头发物理摆动、呼吸起伏、眨眼、状态联动表情，
并为后续换装/换角色预留外部皮肤扩展能力。

---

## 2. 视觉规格

### 2.1 构图

- **姿态**：半身像（头 + 肩 + 胸腰以上），从工具栏上沿探出，身体在栖木线处自然截断。
- **朝向**：微侧身正面朝向用户，温柔治愈感。
- **画风**：日系二次元、清透赛璐璐、柔和渐变阴影、干净线稿、明亮大眼。
- **配色**：低饱和柔色调，向琥珀 `accent #F5A524` 靠拢，适配深色皮肤 `#0E1113`。

### 2.2 尺寸

| 项 | 值 | 说明 |
|---|---|---|
| 设计基准尺寸 | 224 × 256 px | 渲染时缩放到显示尺寸 |
| 宠物显示尺寸 | 112 × 128 px | `PET_DISPLAY_W × PET_DISPLAY_H` |
| 栖木高度 | 136 px | `PET_OVERHANG`，宠物底边贴工具栏上沿 |
| 复合窗尺寸（scale=1） | 212 × 178 px | 宽 = 工具栏 212 px（宠物居中挂正上方，不追加宽度）；高 = 42 + 136 |
| 宠物水平位置 | x = 50 px | `(212 - 112) / 2`，居中于工具栏；右缘 162 px 仍在工具栏内 |

---

## 3. 分层素材

### 3.1 图层 z-order（下 → 上）

| z | id | 文件 | 锚点 | 弹簧参数 | 说明 |
|---|---|---|---|---|---|
| 0 | `HairBack` | `hair_back.png` | (0.5, 0.12) | k=200, c=14, max=3.0°, infl=0.6 | 后发/两侧长发 |
| 1 | `Body` | `body.png` | (0.5, 1.00) | 无 | 躯干 + 服装（含颈肩） |
| 2 | `Head` | `head.png` | (0.5, 0.95) | 无 | 头部轮廓 + 五官底色 |
| 3 | `Face` | `face_*.png` | (0.5, 0.50) | 无 | 表情层，覆盖头部 |
| 4 | `HairFront` | `hair_front.png` | (0.5, 0.10) | k=180, c=12, max=4.5°, infl=0.8 | 刘海/前发 |
| 5 | `Ahoge` | `ahoge.png` | (0.5, 0.05) | k=120, c=8, max=9.0°, infl=1.4 | 呆毛（本角色无呆毛，留空占位） |
| 6 | `Accessory` | — | 视素材 | 视素材 | 预留发饰位 |

**实现状态**：当前默认皮肤使用 0~4 层（HairBack、Body、Head、Face、HairFront），
`Ahoge` 与 `Accessory` 缺失，渲染层自动跳过。

### 3.2 表情集

| 表情 | 文件名 | 当前状态 |
|---|---|---|
| `Normal` | `face_normal.png` | ✅ 正式素材 |
| `Blink` | `face_blink.png` | ✅ 闭眼缝素材 |
| `Smile` | `face_smile.png` | ⚠️ 临时用 `face_normal` 回退 |
| `Focus` | `face_focus.png` | ⚠️ 临时用 `face_normal` 回退 |
| `Surprised` | `face_surprised.png` | ⚠️ 临时用 `face_normal` 回退 |
| `Sleepy` | `face_sleepy.png` | ⚠️ 临时用 `face_normal` 回退 |

> 渲染层已支持 6 种表情并按 `PetClip::face()` 切换；回退表情将在后续素材迭代中替换为 AI 生成专属图。

---

## 4. 物理动画

### 4.1 弹簧

- 算法：半隐式欧拉（symplectic Euler）。
- 激励来源：窗口拖拽位移 → `PetAnim::impulse()`；点击互动额外注入冲量。
- 收敛判定：`value` 与 `velocity` 同时低于阈值 → `needs_tick=false`。
- 各层按 `influence` 分配冲量，形成“前发轻摆、后发微动”的层次感。

### 4.2 呼吸

- 周期：3500 ms。
- 幅度：`breath_amp = 0.012`（相对显示高度，约 ±1.5 px）。
- 作用方式：**整体同步偏移**，所有图层一起轻微上下移动，避免头身脱节。

### 4.3 眨眼

- 随机间隔：2600 ~ 6400 ms。
- 闭眼时长：120 ms。
- 闭眼期间渲染层把 `Face` 层替换为 `FaceExpr::Blink`。

---

## 5. 状态联动表情映射

| 输入法状态 | `PetClip` | 表情 | 备注 |
|---|---|---|---|
| 中文模式 | `ModeCn` | Normal | |
| 英文模式 | `ModeEn` | Sleepy | 当前与 Normal 同图，待替换 |
| 全/半角切换 | `Width` | Surprised | 一闪而过 |
| 简/繁切换 | `Script` | Surprised | 一闪而过 |
| 标点切换 | `Punct` | Surprised | 一闪而过 |
| 闲置 | `Idle` | Normal | |
| 打字中 | `Typing` | Focus | 当前与 Normal 同图，待替换 |
| 点击互动 | `React` | Smile | 当前与 Normal 同图，待替换 |

---

## 6. 三级降级路径

| 级别 | 条件 | 表现 |
|---|---|---|
| **L2（目标）** | 完整分层素材 + 全表情 | 6~7 层 + 物理摆动 + 表情切换 |
| **L1.5（当前默认皮肤）** | 主体分层完整，表情除 Normal/Blink 外为占位 | 头发摆动/眨眼/呼吸均工作，状态表情切换代码就绪但视觉差异待补 |
| **L1（折中）** | 分层切分失败 | 3 层：`hair_back` / 主体（base） / `hair_front`，表情整头切换 |
| **L0（保底）** | 分层素材整体缺失 | 单张 `base.png` + 程序化微动，无分层 |

当前实现已具备 L0 回退路径（`assets/pet/default.png` 像素狗帧表）和 L1.5 默认皮肤。

---

## 7. 外部皮肤格式（扩展接口）

皮肤目录：`%iuv_dir%/pet/skins/<skin_id>/`

```
<skin_id>/
  ├── skin.json
  ├── base.png
  ├── body.png
  ├── head.png
  ├── hair_back.png
  ├── hair_front.png
  ├── ahoge.png        # 可选
  ├── face_normal.png
  ├── face_blink.png
  ├── face_smile.png
  ├── face_focus.png
  ├── face_surprised.png
  └── face_sleepy.png
```

`skin.json` 示例见 `assets/pet/girl_default/skin.json`。
关键字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | 皮肤标识 |
| `design_size` | [u32, u32] | 所有图层 PNG 的基准尺寸 |
| `layers` | array | z-order 图层列表，含 `id`/`anchor`/`spring` |
| `breath_amp` | f32 | 呼吸幅度（相对显示高度） |
| `breath_period_ms` | u32 | 呼吸周期 |
| `blink_interval_ms` | [u32, u32] | 眨眼随机间隔范围 |

> 外部皮肤加载已在内置装配中实现；本次不提供皮肤管理 UI，仅保留目录级扩展能力。

---

## 8. 渲染与命中

- 渲染：`blit_layer` 支持锚点旋转 + 等比缩放，直接在复合 Pixmap 上绘制。
- 色彩：合成在 RGBA 空间完成，最后统一 `pixmap_to_surface` 交换 R/B 为 BGRA。
- 命中：分层合成后抽取宠物区 alpha mask，`PET_HIT_ALPHA = 0x20` 为阈值；透明区鼠标穿透。

---

## 9. 省电与帧率

| 状态 | 定时器间隔 | 说明 |
|---|---|---|
| 动作态（Typing/React/Flash） | 33 ms（30fps） | 必须跟手 |
| 弹簧未收敛 | 33 ms（30fps） | 物理惯性需要 |
| 仅呼吸 + 待眨眼 | 100 ms（10fps） | 慢周期足够平滑 |
| 隐藏 / 失焦 / 完全静止 | KillTimer | 零 CPU |

---

## 10. 验收标准

- [x] 默认皮肤显示为二次元少女半身像，112×128 px。
- [x] 头发前后摆动自然，拖拽时有惯性甩动。
- [x] 呼吸起伏 + 随机眨眼可见。
- [x] 点击少女不透明像素触发互动；透明部分鼠标穿透。
- [x] 中英/全半角/简繁切换、打字状态驱动对应表情/动作代码路径。
- [x] 外部皮肤目录加载接口可用。
- [x] 全量测试通过（> 400 个），无新增依赖。
- [ ] 全部 6 张表情图为专属素材（当前 4 张为占位，后续素材迭代）。
- [ ] 呆毛/发饰层补全（本角色无呆毛，可选）。

---

## 11. 变更记录

| 日期 | 变更内容 | 原因 |
|---|---|---|
| 2026-08-29 | 少女默认皮肤落盘；新增 `PetSkin`/`PetAnim`/`blit_layer` 分层渲染管线 | 形象改造 |
| 2026-08-29 | 表情 `Smile`/`Focus`/`Surprised`/`Sleepy` 暂用 `Normal` 回退 | AI 生成对齐成本，M1 先保证代码路径 |
