//! 消息定义：三个方向枚举 + 线上载荷类型（49 §4.3）。
//!
//! 纪律：
//! - **方向即类型**——C2S 与 S2C 分开定义，错方向的帧在类型上不可表达；
//! - **穷尽 match**——新增变体时两侧 handler 编译期报错；`#[non_exhaustive]` 保证
//!   演进不破坏下游；
//! - `Option` = 「与上帧相同，省带宽」的增量语义，基线规则见 49 §4.5.2；
//! - 载荷字段用定宽整数（u16/u32/i32），不用 usize——线格式不随平台位宽漂移。

use serde::{Deserialize, Serialize};

// ===================== 握手（49 §4.4） =====================

/// 认证密钥：安装时生成、用户配置目录 ACL 保护的共享 token（拍板 §6.4），SHA-256 长度。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth(pub [u8; 32]);

/// 会话重绑令牌：服务端建会话时 `Push::SessionAttached` 下发，重连 `Hello.resume` 回带。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeToken(pub u64);

/// 客户端身份（握手报备 + 服务端日志/排障）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub pid: u32,
    pub tid: u32,
    /// 宿主进程名（如 `weixin.exe`），服务端只作展示。
    pub app: String,
}

/// 能力位图：没协商的能力，对应 PUSH 一律不发（49 §4.4）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps(pub u32);

impl Caps {
    /// 需要候选数据回传 + `Push::UiElement`（游戏内候选栏）。
    pub const UIELEMENT: u32 = 1 << 0;
    /// 需要 `Push::TypingActivity`（桌宠动画）。
    pub const PET: u32 = 1 << 1;

    pub fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

/// 服务端构建标识（排障：一眼看出连的是哪个版本的服务端）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildId(pub String);

// ===================== 按键路径（49 §4.5） =====================

/// 归一化按键（镜像 `iuv_core::Key` 变体集；语义见其注释）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    Char(char),
    /// Shift/CapsLock 字母（大写；仅 TSF 产生）。
    ShiftChar(char),
    Backspace,
    Space,
    Enter,
    Esc,
    Digit(u8),
    Tab,
    Delete,
    Home,
    End,
    Insert,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    /// Alt+←：与左侧相邻候选交换权重（仅 TSF 产生）。
    SwapLeft,
    /// Alt+→：与右侧相邻候选交换权重（仅 TSF 产生）。
    SwapRight,
    /// Shift+Delete：隐藏候选（仅 TSF 产生）。
    HideCandidate,
}

/// 修饰键位（物理键未捕获到的修饰；ShiftChar 等隐式修饰不在此重复）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// TSF 按键回调阶段（处理点唯一化的载体，49 §4.5.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyPhase {
    /// OnTestKeyDown——按键在此阶段被真正处理一次。
    Test,
    /// OnKeyDown——命中 Test 裁定缓存则零 IPC 重放；未命中则现场处理。
    Down,
    /// OnKeyUp（预留，当前不消费）。
    Up,
}

/// 按键令牌：客户端单调 seq，服务端原样回显（应答关联 + 排障）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyToken {
    pub seq: u32,
    pub phase: KeyPhase,
}

/// 热路径应答：正常结果 or 引擎过载快速失败（49 §4.5.2 / §4.7）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum KeyVerdict {
    Consumed(KeyOutcome),
    /// 服务端未推进引擎状态即返回——客户端当场放行本键，**不触发**基线失效重同步。
    Busy,
}

/// 正常处理结果。`Option = None` 表示与上帧相同；**会话首次应答必须全量**
/// （composition 恒 `Some`），双方才有共同基线（49 §4.5.2）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyOutcome {
    /// 应用是否应吞掉本键（OnTestKeyDown 的应答；服务端只收非放行键，恒 true）。
    pub eaten: bool,
    /// 内嵌预编辑（拼音分段）；None = 与上帧相同。
    pub composition: Option<String>,
    /// 切分显示（如 "ni'hao"）。**过渡期字段**：P3 客户端仍自绘候选窗时需要；
    /// 服务端自渲染候选窗落地后移除（届时热路径再瘦 8 字节）。
    pub reading: Option<String>,
    /// Some → 会话结束：`Commit(text)` 上屏文本 / `Cancel` 取消清空。
    pub end: Option<SessionEnd>,
    /// 以下四项仅 `Caps::UIELEMENT`（客户端自绘候选时才有意义；服务端自渲染候选窗，
    /// 普通客户端不消费——49 §4.5.3）。`all_candidates` 为**过渡期字段**（游戏内候选栏
    /// 翻页数据源；P3 客户端自绘期间随应答回传，服务端自渲染落地后移除）。
    pub candidates: Option<Vec<Candidate>>,
    pub all_candidates: Option<Vec<Candidate>>,
    pub page: Option<PageInfo>,
    pub selected: Option<u32>,
}

// ===================== 候选 / 翻页（49 §4.5.3 瘦身版） =====================

/// 候选种类（镜像 `iuv_core::CandidateKind`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateKind {
    Sentence,
    Word,
    Char,
}

/// 线上候选：**只带客户端渲染需要的东西**。引擎侧字段（code/weight/seg_len/score）
/// 不上线——诊断走服务端日志，不占热路径带宽（49 §4.5.3）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
}

/// 翻页信息（usize → u32 定宽）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    pub page: u32,
    pub page_count: u32,
    pub page_size: u32,
    pub total: u32,
}

/// 会话结束方式（镜像 `iuv_core::SessionEnd`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionEnd {
    /// 上屏文本。
    Commit(String),
    /// 取消，不上屏。
    Cancel,
}

/// UI 快照推送载荷（仅 `Push::UiElement` / `KeyOutcome.candidates` 消费）。
/// 镜像 `iuv_core::Effect` 的**瘦身版**：全量候选保留（游戏内候选栏翻页从全量切片），
/// `end` 不上线（EndSession / `KeyOutcome.commit` 覆盖其语义）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    /// 内嵌预编辑：拼音分段（如 "ce'shi"，保留强制分隔符）。
    pub composition: String,
    /// 切分显示，如 "ni'hao"。
    pub reading: String,
    /// 当前页候选（页内索引 0 起）。
    pub candidates: Vec<Candidate>,
    /// 全量候选（所有页，按页内序）——游戏内候选栏数据源。
    pub all_candidates: Vec<Candidate>,
    /// 页内高亮索引。
    pub selected: u32,
    pub page: PageInfo,
}

// ===================== 四态 / 控制（镜像 iuv-win ipc::msg 语义） =====================

/// 中/英（镜像 OPENCLOSE compartment 真相源）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImeMode {
    #[default]
    Chinese,
    English,
}

/// 半角/全角。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImeWidth {
    #[default]
    Half,
    Full,
}

/// 简体/繁体。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImeScript {
    #[default]
    Simplified,
    Traditional,
}

/// 中文标点/英文标点。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImePunct {
    #[default]
    Chinese,
    English,
}

/// 四态（镜像 `iuv_core::ImeState`；iuv-win codec 的 [u8;4] 线编码由 P4 收敛到本定义）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImeState {
    pub mode: ImeMode,
    pub width: ImeWidth,
    pub script: ImeScript,
    pub punct: ImePunct,
}

/// 控制面命令：工具栏点击 → 服务端 → 客户端应用（镜像 iuv-win `CtlCmd`，bool 语义同源：
/// mode 0=中文 1=英文；width 0=半角 1=全角；script 0=简 1=繁；punct 0=中文 1=英文标点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CtlCmd {
    SetMode(bool),
    SetWidth(bool),
    SetScript(bool),
    SetPunct(bool),
    /// 服务端候选窗点击选词（row = 当前页内行号 0-8）：客户端以 Digit(row+1)
    /// 键走远端会话（与数字键同语义），应用后应答。
    CandidateClick(u8),
}

/// Ctl 应用结果（镜像 iuv-win `CtlResult`）：成功回**新**四态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CtlResult {
    Ok { state: ImeState },
    Err,
}

/// 光标矩形（屏幕坐标，服务端定位候选窗用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaretRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// 客户端配置视图（配置纪元变更时随 `Push::ConfigChanged` 下发）。
/// P1 仅含已确认的客户端消费项；P3 接线时按客户端实际消费裁剪扩充。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    /// 新实例初始中/英（客户端本地 OPENCLOSE 初值，28-initial-state-settings.md）。
    pub initial_mode: ImeMode,
}

// ===================== 用户库写（原数据面管道迁入） =====================

/// 用户库写操作（镜像 `iuv_core::UserMutation`，对应 UserDict Swap/Set/Remove/Block）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserMutation {
    /// Shift+←/→ 主动调权：a/b 两词互写对方合成权重（绝对值覆盖，双 code 签名）。
    Swap {
        a_code: String,
        a_word: String,
        a_eff: u32,
        b_code: String,
        b_word: String,
        b_eff: u32,
    },
    /// 自造词/覆盖写入（upsert）。
    Set {
        code: String,
        word: String,
        adj: u32,
    },
    /// 移除用户库条目（隐藏自造词/覆盖 = 撤销自造）。
    Remove { code: String, word: String },
    /// 屏蔽基础库词条（Shift+Delete 隐藏）。
    Block { code: String, word: String },
}

// ===================== 错误模型（49 §4.8） =====================

/// 协议级错误。类型化、带字段——对照 weasel `_ThrowLastError → catch(DWORD) → return 0`
/// （错误与成功返回 0 不可区分）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtoError {
    VersionMismatch {
        client: u16,
        server: u16,
    },
    /// 认证失败——立即断连，不重试（49 §4.4）。
    Unauthenticated,
    FrameTooLarge {
        len: u32,
        max: u32,
    },
    /// 解码即拒：offset = 出错字节在载荷内的偏移；reason 人读。
    Malformed {
        offset: u32,
        reason: String,
    },
    /// 未知 kind 或未知枚举变体（serde 侧解码失败的类型化包装）。
    UnknownTag {
        kind: u8,
        tag: u16,
    },
    /// 热路径截止时间已过（op = 消息名，elapsed_ms 供日志）。
    Deadline {
        op: String,
        elapsed_ms: u32,
    },
    /// 连接对端已消失（管道断开 / 心跳判死）。
    ServerGone,
}

// ===================== 三个方向枚举（49 §4.3） =====================

// —— C2S：客户端 → 服务端 ——
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum C2S {
    /// 握手。连接后第一条帧，必须是它（49 §4.4）。`caps` = 客户端请求的能力位
    /// （服务端按自身支持集取交集后回 `HelloAck.caps`）。
    Hello {
        proto_min: u16,
        proto_max: u16,
        auth: Auth,
        caps: Caps,
        resume: Option<ResumeToken>,
        client: ClientInfo,
    },
    /// 热路径：处理一次按键，**引擎的唯一调用入口**。
    /// `full` = 基线已失效（Deadline/传输错误后），要求下一次应答强制全量。
    Key {
        key: Key,
        mods: Mods,
        token: KeyToken,
        full: bool,
    },
    /// 会话生命周期（起于首键、终于上屏/取消）。
    EndSession {
        end: SessionEnd,
    },
    /// 光标矩形（低频；客户端节流后发送）。
    CaretMoved {
        rect: CaretRect,
        dpi: u16,
    },
    FocusChanged {
        focused: bool,
    },
    /// 四态同步（连接建立时 + 每次变化；客户端是 OPENCLOSE 真相源，服务端会话运行时消费）。
    ImeState(ImeState),
    /// 维护模式（对照 weasel START/END_MAINTENANCE）。
    SetMaintenance {
        on: bool,
    },
    /// 用户库写（原数据面管道迁入）。
    UserMutation(UserMutation),
    /// Ctl 应用结果（控制面应答，镜像 iuv-win CtlResult）。
    CtlResult(CtlResult),
    /// 打字活动（桌宠动画驱动；原 toolbar signal Typing 迁入）。
    TypingActivity { active: bool },
    /// 语言栏菜单：打开设置页（原数据面管道 OpenSettings 迁入，fire-and-forget）。
    OpenSettings,
    /// 语言栏菜单：切换工具栏显隐（fire-and-forget）。
    ToggleToolbar,
    /// 语言栏菜单：查询工具栏显隐（菜单文案动态化，需应答）。
    ToolbarVisibleQuery,
    /// 隐藏服务端候选窗（会话**不**结束——焦点切换不打断会话原则的远端对应：
    /// 本地窗 OnSetFocus 隐藏时，server 侧窗口同步隐藏，回焦后下键自然重显）。
    CandwinHide,
    Ping {
        nonce: u32,
    },
    Pong {
        nonce: u32,
    },
    /// 通用肯定应答（UserMutation ack 等）。
    Ok,
    /// 通用否定应答。
    Err(ProtoError),
}

// —— S2C：服务端 → 客户端 ——
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum S2C {
    /// 握手应答：定版 + 能力集。
    HelloAck {
        proto: u16,
        caps: Caps,
        server_build: BuildId,
    },
    /// 热路径应答。
    KeyResult(KeyVerdict),
    Ok,
    Err(ProtoError),
    /// 控制面请求（服务端发起，需应答；原 ctl 反向通道）。
    Ctl {
        cmd: CtlCmd,
    },
    Ping {
        nonce: u32,
    },
    Pong {
        nonce: u32,
    },
    /// 工具栏显隐查询应答（`C2S::ToolbarVisibleQuery`；daemon 查询迁移）。
    ToolbarVisible { visible: bool },
}

// —— PUSH：服务端 → 客户端 单向推送（latest-wins，49 §4.6）——
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Push {
    /// 新会话建立，下发重绑令牌（49 §4.4；唯一在服务端**建会话时**发的 PUSH）。
    SessionAttached { token: ResumeToken },
    /// 四态变化（原 toolbar signal：FocusGained/StateChanged 合并为此）。
    ImeState(ImeState),
    /// 焦点绑定。
    FocusBound { focused: bool },
    /// 配置纪元变更。
    ConfigChanged {
        epoch: u32,
        client_view: ClientConfig,
    },
    /// 用户库版本变更（只推版本号，数据仍走 SHM，49 §4.9）。
    UserDictChanged { version: u32 },
    /// 游戏内候选（仅 `Caps::UIELEMENT`）。
    UiElement(Effect),
    /// 打字活动（桌宠动画，仅 `Caps::PET`）。
    TypingActivity { active: bool },
    /// 服务端即将退出（优雅停机）。**唯一不可合并、必达**的推送。
    Shutdown { grace_ms: u32 },
}

/// 载荷信封：kind 字节（[`frame::FrameKind`]）与变体一一对应，帧自描述——
/// 不依赖连接方向即可解码（repl 抓包/测试直读）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    /// kind=0：客户端请求。
    ClientReq(C2S),
    /// kind=1：服务端请求（仅 `Ctl` / `Ping`）。
    ServerReq(S2C),
    /// kind=2：客户端应答（仅 `Ok` / `Err` / `CtlResult` / `Pong`）。
    ClientResp(C2S),
    /// kind=3：服务端应答。
    ServerResp(S2C),
    /// kind=4：服务端推送。
    Push(Push),
}
