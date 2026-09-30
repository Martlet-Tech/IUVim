//! 实例四态 + 控制面命令（③-2 镜像归一：全仓唯一定义，iuv-core/iuv-proto/iuv-win
//! 三方 re-export）。28-initial-state-settings.md / 32-status-toolbar.md §5.1。
//!
//! 双重语义（P3.3 起 `InitialState`/`RuntimeState` 合并为单类型 `ImeState`）：
//! - **新实例初始状态**（core `Config.initial_state` 配置节点）：中/英激活强制设默认；
//!   半角/全角、简体/繁体已生效。默认 = 主流：中文/半角/简体/中文标点。
//! - **实例运行时值**：每个 TSF 实例持有自己的 `Arc<Mutex<ImeState>>`（live 读），
//!   工具栏/会话外操作修改只影响本实例；中英字段镜像 OPENCLOSE compartment 真相源，
//!   其余三字段由工具栏 `CtlCmd::Set*` 写入。

use serde::{Deserialize, Serialize};

/// 中/英（镜像 OPENCLOSE compartment 真相源）。中文默认 = 激活即打开（MS IME 同款语义）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImeMode {
    /// 中文（默认）：激活后输入法为中文模式
    #[default]
    Chinese,
    /// 英文：每个新 TSF 实例从英文模式起（Ctrl+Space 可切回中文）
    English,
}

/// 半角/全角。半角默认；全角行为已落地（punct.rs fullwidth + 会话 to_output 钩子）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImeWidth {
    /// 半角（默认）
    #[default]
    Half,
    /// 全角
    Full,
}

/// 简体/繁体。简体默认；繁体生效 = 简体词库 + 运行时简→繁转换
/// （31-script-traditional.md；数据文件 `iuv.opencc` 缺失时降级简体输出）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImeScript {
    /// 简体（默认）
    #[default]
    Simplified,
    /// 繁体（简体词库 + 运行时简→繁转换）
    Traditional,
}

/// 中文状态标点风格（中文标点/英文标点）。替代旧顶层 `english_punctuation: bool`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImePunct {
    /// 中文标点（默认，全角：`，`/`。`；主流输入法默认）
    #[default]
    Chinese,
    /// 英文标点（中文状态按标点键直通英文形：`，`→`,`）
    English,
}

/// 实例四态（每 TSF 实例，32-status-toolbar.md §5.1）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImeState {
    /// 中/英（镜像 OPENCLOSE compartment：`ImeMode::Chinese` = 打开）
    pub mode: ImeMode,
    /// 半角/全角
    pub width: ImeWidth,
    /// 简体/繁体
    pub script: ImeScript,
    /// 中文标点/英文标点
    pub punct: ImePunct,
}

/// 四态唯一线编码（管道传输；字段序 mode/width/script/punct，见 iuv-win codec.rs）：
/// `mode` 0=中文 1=英文；`width` 0=半角 1=全角；`script` 0=简体 1=繁体；`punct` 0=中文标点 1=英文标点。
/// 全仓唯一映射点——加第五态只改这里 + codec 一个函数。
impl From<ImeState> for [u8; 4] {
    fn from(s: ImeState) -> Self {
        [
            match s.mode {
                ImeMode::Chinese => 0,
                ImeMode::English => 1,
            },
            match s.width {
                ImeWidth::Half => 0,
                ImeWidth::Full => 1,
            },
            match s.script {
                ImeScript::Simplified => 0,
                ImeScript::Traditional => 1,
            },
            match s.punct {
                ImePunct::Chinese => 0,
                ImePunct::English => 1,
            },
        ]
    }
}

/// 线字节 → 四态。任一字节非 0/1 → `Err`（解码侧拒绝非法值，不静默收垃圾）。
impl TryFrom<[u8; 4]> for ImeState {
    type Error = ();

    fn try_from(b: [u8; 4]) -> Result<Self, ()> {
        fn pick<T>(v: u8, zero: T, one: T) -> Result<T, ()> {
            match v {
                0 => Ok(zero),
                1 => Ok(one),
                _ => Err(()),
            }
        }
        Ok(ImeState {
            mode: pick(b[0], ImeMode::Chinese, ImeMode::English)?,
            width: pick(b[1], ImeWidth::Half, ImeWidth::Full)?,
            script: pick(b[2], ImeScript::Simplified, ImeScript::Traditional)?,
            punct: pick(b[3], ImePunct::Chinese, ImePunct::English)?,
        })
    }
}

/// 控制面命令：工具栏点击/语言栏 → 四态翻转 + 候选窗点击选词。bool 语义同源：
/// mode 0=中文 1=英文；width 0=半角 1=全角；script 0=简 1=繁；punct 0=中文 1=英文标点。
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

/// Ctl 应用结果：成功回**新**四态；失败带人读原因（客户端日志/server 日志透传）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CtlResult {
    Ok { state: ImeState },
    Err { msg: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrip() {
        for state in [
            ImeState::default(),
            ImeState {
                mode: ImeMode::English,
                width: ImeWidth::Full,
                script: ImeScript::Traditional,
                punct: ImePunct::English,
            },
            ImeState {
                mode: ImeMode::English,
                ..ImeState::default()
            },
        ] {
            assert_eq!(ImeState::try_from(<[u8; 4]>::from(state)), Ok(state));
        }
    }

    #[test]
    fn wire_encoding_order() {
        // 字段序 mode/width/script/punct：全英/全/繁/英标 = 全 1。
        let all_one = <[u8; 4]>::from(ImeState {
            mode: ImeMode::English,
            width: ImeWidth::Full,
            script: ImeScript::Traditional,
            punct: ImePunct::English,
        });
        assert_eq!(all_one, [1, 1, 1, 1]);
        assert_eq!(<[u8; 4]>::from(ImeState::default()), [0, 0, 0, 0]);
    }

    #[test]
    fn wire_rejects_invalid_byte() {
        assert!(ImeState::try_from([0, 0, 0, 2]).is_err());
        assert!(ImeState::try_from([7, 0, 0, 0]).is_err());
        assert!(ImeState::try_from([0, 0, 0xFF, 0]).is_err());
    }
}
