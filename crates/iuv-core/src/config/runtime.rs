//! 实例四态（`initial_state` 配置节点 + 运行时值）：28-initial-state-settings.md
//! + 32-status-toolbar.md §5.1。P2.1 从 config/mod.rs 拆出，P3.3 合并
//! `InitialState`/`RuntimeState` 为单类型 `ImeState`（字段同构无发散，删机械 From/重复 Default）。

use crate::config::{InitialMode, PunctMode, ScriptMode, WidthMode};

/// 实例四态（每 TSF 实例，`docs/plan/32-status-toolbar.md` §5.1）。
///
/// 双重语义：
/// - **新实例初始状态**（`initial_state` 配置节点，28-initial-state-settings.md）：
///   中/英激活强制设默认；半角/全角、简体/繁体已生效（31-script-traditional.md：繁体 = 运行时转换）。
///   默认 = 主流：中文/半角/简体/中文标点（与旧版零行为变化）。
/// - **实例运行时值**：每个 TSF 实例（一个窗口/线程的 TextService）持有自己的
///   `Arc<Mutex<ImeState>>`（live 读，非快照），工具栏/会话外操作修改只影响本实例；
///   Alt+Tab 往返保留；设置页初始值仅在新实例创建时生效一次。点简繁/全半角
///   当前候选/预编辑立即重渲 = 用户已确认。中英字段镜像 OPENCLOSE compartment 真相源
///   （OnChange 统一写），其余三字段由工具栏 CtlCmd::Set* 写入。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ImeState {
    /// 中/英（镜像 OPENCLOSE compartment：`InitialMode::Chinese` = 打开）
    pub mode: InitialMode,
    /// 半角/全角
    pub width: WidthMode,
    /// 简体/繁体
    pub script: ScriptMode,
    /// 中文标点/英文标点
    pub punct: PunctMode,
}

/// 四态唯一线编码（管道传输；字段序 mode/width/script/punct，见 iuv-win codec.rs）：
/// `mode` 0=中文 1=英文；`width` 0=半角 1=全角；`script` 0=简体 1=繁体；`punct` 0=中文标点 1=英文标点。
/// 全仓唯一映射点——加第五态只改这里 + codec 一个函数。
impl From<ImeState> for [u8; 4] {
    fn from(s: ImeState) -> Self {
        [
            match s.mode {
                InitialMode::Chinese => 0,
                InitialMode::English => 1,
            },
            match s.width {
                WidthMode::Half => 0,
                WidthMode::Full => 1,
            },
            match s.script {
                ScriptMode::Simplified => 0,
                ScriptMode::Traditional => 1,
            },
            match s.punct {
                PunctMode::Chinese => 0,
                PunctMode::English => 1,
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
            mode: pick(b[0], InitialMode::Chinese, InitialMode::English)?,
            width: pick(b[1], WidthMode::Half, WidthMode::Full)?,
            script: pick(b[2], ScriptMode::Simplified, ScriptMode::Traditional)?,
            punct: pick(b[3], PunctMode::Chinese, PunctMode::English)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrip() {
        for state in [
            ImeState::default(),
            ImeState {
                mode: InitialMode::English,
                width: WidthMode::Full,
                script: ScriptMode::Traditional,
                punct: PunctMode::English,
            },
            ImeState {
                mode: InitialMode::English,
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
            mode: InitialMode::English,
            width: WidthMode::Full,
            script: ScriptMode::Traditional,
            punct: PunctMode::English,
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