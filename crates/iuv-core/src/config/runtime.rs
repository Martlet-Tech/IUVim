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
///   （OnChange 统一写），其余三字段由工具栏 Cmd::SetState 写入。
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

impl ImeState {
    /// 工具栏四态 → 管道传输值（u8 编码；字段序 mode/width/script/punct，见 iuv-win ipc.rs）。
    /// 返回裸元组而非 `ToolbarState`（iuv-core 不构造传输结构，解耦依赖边）。
    pub fn to_toolbar(&self) -> (u8, u8, u8, u8) {
        (
            match self.mode {
                InitialMode::Chinese => 0,
                InitialMode::English => 1,
            },
            match self.width {
                WidthMode::Half => 0,
                WidthMode::Full => 1,
            },
            match self.script {
                ScriptMode::Simplified => 0,
                ScriptMode::Traditional => 1,
            },
            match self.punct {
                PunctMode::Chinese => 0,
                PunctMode::English => 1,
            },
        )
    }

    /// 单字段写入（工具栏 Cmd::SetState：field 0=mode 1=width 2=script 3=punct，value 0/1）。
    /// 返回 true = 字段合法且已修改。mode 字段不经此路径（走 OPENCLOSE compartment）。
    pub fn set_field(&mut self, field: u8, value: u8) -> bool {
        let v = value != 0;
        match field {
            0 => {
                self.mode = if v {
                    InitialMode::English
                } else {
                    InitialMode::Chinese
                };
                true
            }
            1 => {
                self.width = if v {
                    WidthMode::Full
                } else {
                    WidthMode::Half
                };
                true
            }
            2 => {
                self.script = if v {
                    ScriptMode::Traditional
                } else {
                    ScriptMode::Simplified
                };
                true
            }
            3 => {
                self.punct = if v {
                    PunctMode::English
                } else {
                    PunctMode::Chinese
                };
                true
            }
            _ => false,
        }
    }
}