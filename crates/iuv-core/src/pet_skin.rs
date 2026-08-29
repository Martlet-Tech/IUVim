//! 桌宠皮肤数据模型（**纯数据类型**，serde 可反序列化）。
//!
//! 本模块只定义数据，不含渲染/平台逻辑，供 `iuv-ui`（渲染）与 `iuv-daemon`（加载）共享，
//! 且不引入任何新的单向依赖。
//!
//! # `PetSkin` 是换装/换角色的扩展点
//!
//! 新增一个皮肤 = 新增一份描述 + 一组图层 PNG，**无需改动渲染代码**。素材来源有两路，
//! 均由 daemon 侧装配（本模块不感知）：
//!
//! - **内置默认皮肤**：Rust `const` 描述 + `include_bytes!` 内嵌（开箱可用、零外部依赖）
//! - **外部皮肤目录**：`<iuv_dir>/pet/skins/<skin_id>/skin.json` + 各图层 PNG（免重编译换装）
//!
//! # 坐标与单位约定
//!
//! - `anchor`：归一化锚点 `(x, y)`，取值 0..1，相对**图层自身**位图；绕它做旋转/缩放。
//!   用归一化值而非像素，是为了让不同 `design_size` 的皮肤复用同一套锚点语义。
//! - `breath_amp`：**归一化幅度**（相对设计高度的比例，0..1），渲染时乘显示高度换算为像素。
//!   用比例而非绝对像素，保证缩放后呼吸观感一致。
//! - 角度单位统一为**度**（deg），渲染层自行转弧度。

use serde::{Deserialize, Serialize};

/// 图层标识（z-order 由下至上，与 [`LayerId::ALL`] 顺序一致）。
///
/// 变体顺序即**渲染顺序**：`HairBack` 最底、`Accessory` 最上。素材文件名由
/// [`LayerId::file_stem`] 约定，外部皮肤目录按此命名即可被自动识别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LayerId {
    /// 后发 / 长发的背景部分（在身体之后）
    HairBack,
    /// 身体 + 服装（含颈肩）
    Body,
    /// 头部（脸型，不含眼嘴）
    Head,
    /// 表情层（眼睛 + 嘴，按 [`FaceExpr`] 多帧切换）
    Face,
    /// 刘海 / 前发（覆盖脸部上方）
    HairFront,
    /// 呆毛（摆动幅度最大的部件）
    Ahoge,
    /// 发饰等可选挂件
    Accessory,
}

impl LayerId {
    /// 图层总数（用作定长数组容量）。
    pub const COUNT: usize = 7;

    /// 全部图层，按 z-order 由下至上排列。
    pub const ALL: [LayerId; Self::COUNT] = [
        LayerId::HairBack,
        LayerId::Body,
        LayerId::Head,
        LayerId::Face,
        LayerId::HairFront,
        LayerId::Ahoge,
        LayerId::Accessory,
    ];

    /// 数组下标（与 [`LayerId::ALL`] 中的位次一致）。
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// 素材文件名主干（外部皮肤目录约定：`<file_stem>.png`）。
    ///
    /// 表情层特殊：文件名带表情后缀，形如 `face_normal.png` / `face_blink.png`，
    /// 由 [`FaceExpr::file_name`] 生成。
    #[inline]
    pub fn file_stem(self) -> &'static str {
        match self {
            LayerId::HairBack => "hair_back",
            LayerId::Body => "body",
            LayerId::Head => "head",
            LayerId::Face => "face",
            LayerId::HairFront => "hair_front",
            LayerId::Ahoge => "ahoge",
            LayerId::Accessory => "accessory",
        }
    }
}

/// 表情（对应表情层的一类素材帧）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FaceExpr {
    /// 常态
    Normal,
    /// 闭眼（眨眼瞬间）
    Blink,
    /// 微笑
    Smile,
    /// 专注（打字中）
    Focus,
    /// 惊讶（点击互动 / 状态切换）
    Surprised,
    /// 打盹（英文模式）
    Sleepy,
}

impl FaceExpr {
    /// 表情总数（用作定长数组容量）。
    pub const COUNT: usize = 6;

    /// 全部表情（顺序即素材帧序，供外部皮肤按序提供）。
    pub const ALL: [FaceExpr; Self::COUNT] = [
        FaceExpr::Normal,
        FaceExpr::Blink,
        FaceExpr::Smile,
        FaceExpr::Focus,
        FaceExpr::Surprised,
        FaceExpr::Sleepy,
    ];

    /// 数组下标（与 [`FaceExpr::ALL`] 中的位次一致）。
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// 表情层素材文件名（含扩展名），形如 `face_normal.png`。
    #[inline]
    pub fn file_name(self) -> &'static str {
        match self {
            FaceExpr::Normal => "face_normal.png",
            FaceExpr::Blink => "face_blink.png",
            FaceExpr::Smile => "face_smile.png",
            FaceExpr::Focus => "face_focus.png",
            FaceExpr::Surprised => "face_surprised.png",
            FaceExpr::Sleepy => "face_sleepy.png",
        }
    }
}

/// 默认弹簧刚度（ω² ≈ 180 → 摆频约 2.1 Hz，接近真人发丝摆动）。
pub const DEFAULT_STIFFNESS: f32 = 180.0;
/// 默认弹簧阻尼（ζ ≈ 0.45，欠阻尼：摆几下后收敛，不会一直晃）。
pub const DEFAULT_DAMPING: f32 = 12.0;

/// 弹簧参数（驱动该图层的摆动）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpringParam {
    /// 刚度 k：越大回弹越快、频率越高。
    pub stiffness: f32,
    /// 阻尼 c：越大越快静止。`c = 2·ζ·√k`，ζ<1 欠阻尼（会摆过头）。
    pub damping: f32,
    /// 最大摆角（度）：对弹簧输出做限幅，防止夸张或穿模。
    pub max_angle_deg: f32,
    /// 对整体运动（窗口拖拽）的敏感度：冲量乘以该系数后注入本层。
    pub influence: f32,
}

impl Default for SpringParam {
    fn default() -> Self {
        SpringParam {
            stiffness: DEFAULT_STIFFNESS,
            damping: DEFAULT_DAMPING,
            max_angle_deg: 4.0,
            influence: 1.0,
        }
    }
}

/// 单个图层定义。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PetLayer {
    /// 图层标识（决定 z-order 与素材文件名）
    pub id: LayerId,
    /// 归一化锚点（旋转/缩放支点），0..1 相对本层位图
    pub anchor: (f32, f32),
    /// 摆动参数；`None` = 该层不做物理摆动（如身体、头部）
    #[serde(default)]
    pub spring: Option<SpringParam>,
}

fn default_breath_amp() -> f32 {
    0.012
}
fn default_breath_period_ms() -> u32 {
    3500
}
fn default_blink_interval_ms() -> (u32, u32) {
    (2600, 6400)
}

/// 一套角色皮肤（桌宠形象的全部静态描述）。
///
/// `layers` 的**数组顺序即渲染顺序**（由下至上），加载方不做重排序。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PetSkin {
    /// 皮肤标识（外部皮肤目录名）
    pub id: String,
    /// 设计基准尺寸（宽, 高）；渲染时等比缩放到显示尺寸
    pub design_size: (u32, u32),
    /// 图层清单，顺序即 z-order（由下至上）
    pub layers: Vec<PetLayer>,
    /// 呼吸幅度（归一化，相对设计高度）
    #[serde(default = "default_breath_amp")]
    pub breath_amp: f32,
    /// 呼吸周期（毫秒）
    #[serde(default = "default_breath_period_ms")]
    pub breath_period_ms: u32,
    /// 随机眨眼间隔区间（毫秒，闭区间）
    #[serde(default = "default_blink_interval_ms")]
    pub blink_interval_ms: (u32, u32),
}

impl PetSkin {
    /// 查图层定义（缺失 → `None`，渲染层据此跳过该层）。
    pub fn layer(&self, id: LayerId) -> Option<&PetLayer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// 内置默认皮肤（少女半身像）。
    ///
    /// 锚点按"半身像"构图取：身体锚在栖木线（底边中点），头部锚在颈部，
    /// 头发/呆毛锚在头顶发根——这样摆动时根部固定、发梢甩动，符合物理直觉。
    ///
    /// 摆动强度按部件递增：后发 < 前发 < 呆毛（呆毛最夸张，是 Live2D 观感的主要来源）。
    pub fn builtin_girl_default() -> PetSkin {
        PetSkin {
            id: "girl_default".to_string(),
            // 显示尺寸 112×128 的 2 倍（清晰度与内嵌体积的平衡点）
            design_size: (224, 256),
            layers: vec![
                PetLayer {
                    id: LayerId::HairBack,
                    anchor: (0.5, 0.12),
                    spring: Some(SpringParam {
                        max_angle_deg: 3.0,
                        influence: 0.55,
                        ..SpringParam::default()
                    }),
                },
                PetLayer {
                    id: LayerId::Body,
                    anchor: (0.5, 1.0),
                    spring: None,
                },
                PetLayer {
                    id: LayerId::Head,
                    anchor: (0.5, 0.95),
                    spring: None,
                },
                PetLayer {
                    id: LayerId::Face,
                    anchor: (0.5, 0.5),
                    spring: None,
                },
                PetLayer {
                    id: LayerId::HairFront,
                    anchor: (0.5, 0.10),
                    spring: Some(SpringParam {
                        max_angle_deg: 4.5,
                        influence: 0.8,
                        ..SpringParam::default()
                    }),
                },
                PetLayer {
                    id: LayerId::Ahoge,
                    anchor: (0.5, 0.05),
                    spring: Some(SpringParam {
                        // 呆毛更软（刚度低）、摆幅更大、更敏感 → 甩动最明显
                        stiffness: 120.0,
                        damping: 8.0,
                        max_angle_deg: 9.0,
                        influence: 1.4,
                    }),
                },
            ],
            breath_amp: default_breath_amp(),
            breath_period_ms: default_breath_period_ms(),
            blink_interval_ms: default_blink_interval_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_id_index_matches_all_order() {
        for (i, id) in LayerId::ALL.iter().enumerate() {
            assert_eq!(id.index(), i, "index 必须与 ALL 位次一致");
        }
        assert_eq!(LayerId::COUNT, LayerId::ALL.len());
    }

    #[test]
    fn layer_id_file_stem_is_stable_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for id in LayerId::ALL {
            let stem = id.file_stem();
            assert!(!stem.is_empty());
            assert!(
                seen.insert(stem),
                "file_stem 必须唯一，重复：{stem}"
            );
        }
    }

    #[test]
    fn face_expr_file_name_is_prefixed_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for e in FaceExpr::ALL {
            let name = e.file_name();
            assert!(name.starts_with("face_"), "{name} 应以 face_ 开头");
            assert!(name.ends_with(".png"), "{name} 应以 .png 结尾");
            assert!(seen.insert(name), "表情文件名重复：{name}");
        }
        assert_eq!(FaceExpr::ALL.len(), 6);
    }

    #[test]
    fn face_expr_index_matches_all_order() {
        for (i, e) in FaceExpr::ALL.iter().enumerate() {
            assert_eq!(e.index(), i);
        }
    }

    #[test]
    fn builtin_skin_layers_are_in_z_order() {
        let skin = PetSkin::builtin_girl_default();
        // 内置皮肤不含挂件层，但顺序必须是 ALL 的子序列（z-order 由下至上）
        let mut last = None;
        for layer in &skin.layers {
            if let Some(prev) = last {
                assert!(
                    prev < layer.id.index(),
                    "图层顺序违反 z-order：{:?} 应在 {:?} 之前",
                    prev,
                    layer.id
                );
            }
            last = Some(layer.id.index());
        }
    }

    #[test]
    fn builtin_skin_layer_lookup_hits() {
        let skin = PetSkin::builtin_girl_default();
        for id in [LayerId::Body, LayerId::Head, LayerId::Face, LayerId::Ahoge] {
            assert!(skin.layer(id).is_some(), "{id:?} 必须存在于内置皮肤");
        }
        assert!(skin.layer(LayerId::Accessory).is_none(), "内置皮肤不含挂件层");
    }

    #[test]
    fn builtin_skin_swing_layers_have_spring() {
        let skin = PetSkin::builtin_girl_default();
        for id in [LayerId::HairBack, LayerId::HairFront, LayerId::Ahoge] {
            let layer = skin.layer(id).expect("摆动层必须存在");
            assert!(layer.spring.is_some(), "{id:?} 应有摆动参数");
        }
        for id in [LayerId::Body, LayerId::Head, LayerId::Face] {
            let layer = skin.layer(id).expect("层必须存在");
            assert!(layer.spring.is_none(), "{id:?} 不应摆动");
        }
    }

    #[test]
    fn builtin_skin_swing_strength_increases_by_part() {
        let skin = PetSkin::builtin_girl_default();
        let back = skin.layer(LayerId::HairBack).unwrap().spring.unwrap();
        let front = skin.layer(LayerId::HairFront).unwrap().spring.unwrap();
        let ahoge = skin.layer(LayerId::Ahoge).unwrap().spring.unwrap();
        assert!(
            back.max_angle_deg < front.max_angle_deg
                && front.max_angle_deg < ahoge.max_angle_deg,
            "摆幅应递增：后发 < 前发 < 呆毛"
        );
    }

    #[test]
    fn skin_json_roundtrip() {
        let skin = PetSkin::builtin_girl_default();
        let json = serde_json::to_string(&skin).expect("序列化");
        let back: PetSkin = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(skin, back);
    }

    #[test]
    fn skin_json_omits_optional_fields_with_defaults() {
        // 外部皮肤可省略可选字段（呼吸/眨眼），应套用默认值
        let json = r#"{
            "id": "minimal",
            "design_size": [256, 256],
            "layers": [{"id": "Body", "anchor": [0.5, 1.0]}]
        }"#;
        let skin: PetSkin = serde_json::from_str(json).expect("最小 JSON 应可解析");
        assert_eq!(skin.id, "minimal");
        assert_eq!(skin.design_size, (256, 256));
        assert_eq!(skin.layers.len(), 1);
        assert!(skin.layers[0].spring.is_none(), "省略 spring → None");
        assert_eq!(skin.breath_amp, default_breath_amp());
        assert_eq!(skin.breath_period_ms, default_breath_period_ms());
        assert_eq!(skin.blink_interval_ms, default_blink_interval_ms());
    }

    #[test]
    fn spring_param_default_is_underdamped() {
        // ζ = c / (2√k) 应 < 1（欠阻尼才会摆动），且明显 > 0（不能一直晃）
        let p = SpringParam::default();
        let zeta = p.damping / (2.0 * p.stiffness.sqrt());
        assert!(zeta > 0.1 && zeta < 1.0, "阻尼比应在 (0.1, 1.0)，实际 {zeta}");
    }
}
