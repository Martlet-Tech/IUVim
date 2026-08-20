//! 管道消息类型（P3.2 自 iuv-data/ipc.rs 移入 iuv-win）：Request/Response + 工具栏四态
//! + 反向控制通道 Cmd/Result。编码见 `super::codec`，传输见 `super::pipe`/`super::ctl`。

/// 会话进程 → 守护进程的写请求（编码表见 `codec.rs`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Shift+←/→ 主动调权：a/b 两词互写对方合成权重（双 code 签名，见 UserDict::apply_swap）。
    Swap {
        a_code: String,
        a_word: String,
        a_adj: u32,
        b_code: String,
        b_word: String,
        b_adj: u32,
    },
    /// 自造词/覆盖写入（upsert）。
    Set { code: String, word: String, adj: u32 },
    /// 移除用户库条目（隐藏自造词/覆盖 = 撤销自造）。
    Remove { code: String, word: String },
    /// 屏蔽基础库词条（Shift+Delete 隐藏）。
    Block { code: String, word: String },
    /// 健康检查：探测 daemon 在线 + 拿当前 version。
    Ping,
    /// M6 语言栏菜单「设置」：通知 daemon 打开设置页（不触碰用户库）。
    OpenSettings,
    /// M6 语言栏菜单/卸载脚本：通知 daemon 干净退出（写盘后退出）。
    Quit,
    /// 32-status-toolbar.md §4.1：TSF 实例 Activate 时注册 + 上报初始四态。
    /// daemon 记入实例表（pid:tid 唯一），供看板判定/点击寻址。
    Register {
        pid: u32,
        tid: u32,
        state: ToolbarState,
    },
    /// 32-status-toolbar.md §4.1：实例运行时四态变化上报（OPENCLOSE OnChange /
    /// Cmd::SetState 应用成功后）。
    StateSync {
        pid: u32,
        tid: u32,
        state: ToolbarState,
    },
    /// 32-status-toolbar.md §4.1：Activate/Deactivate 通知（daemon 判「iuv 被选中」）。
    Active { pid: u32, tid: u32, active: bool },
    /// 32-status-toolbar.md §4.1：语言栏右键菜单「显示/隐藏工具栏」（全局偏好切换）。
    ToggleToolbar,
    /// 32-status-toolbar.md §4.1：实例 Drop 注销（从实例表移除）。
    Unregister { pid: u32, tid: u32 },
}

/// 守护进程 → 会话进程的响应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Ok { version: u32 },
    Err { msg: String },
}

// ===== 32-status-toolbar.md 工具栏四态 + 反向控制通道 =====

/// 工具栏四态传输值（每 TSF 实例，32-status-toolbar.md §2.4/§4）。
/// u8 编码（与 iuv-core `ImeState::to_toolbar` 一致）：
/// `mode` 0=中文 1=英文；`width` 0=半角 1=全角；`script` 0=简体 1=繁体；`punct` 0=中文标点 1=英文标点。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolbarState {
    pub mode: u8,
    pub width: u8,
    pub script: u8,
    pub punct: u8,
}

impl From<(u8, u8, u8, u8)> for ToolbarState {
    /// iuv-core `ImeState::to_toolbar()` 的裸元组 → 传输结构（字段序 mode/width/script/punct）。
    fn from((mode, width, script, punct): (u8, u8, u8, u8)) -> Self {
        ToolbarState {
            mode,
            width,
            script,
            punct,
        }
    }
}

impl ToolbarState {
    #[cfg(test)]
    pub(crate) const fn new(mode: u8, width: u8, script: u8, punct: u8) -> Self {
        ToolbarState {
            mode,
            width,
            script,
            punct,
        }
    }

    /// 读单字段（field 0=mode 1=width 2=script 3=punct；非法 → 0）。
    pub fn field(&self, field: u8) -> u8 {
        match field {
            0 => self.mode,
            1 => self.width,
            2 => self.script,
            3 => self.punct,
            _ => 0,
        }
    }
}

/// 反向控制通道字段 id（daemon → TSF 的 Cmd::SetState 用）。
pub const CTL_FIELD_MODE: u8 = 0;
pub const CTL_FIELD_WIDTH: u8 = 1;
pub const CTL_FIELD_SCRIPT: u8 = 2;
pub const CTL_FIELD_PUNCT: u8 = 3;

/// 反向控制通道管道名前缀：`\\.\pipe\iuv-ctl-<pid>-<tid>`。
const CTL_PIPE_PREFIX: &str = r"\\.\pipe\iuv-ctl";

/// 实例控制管道完整名（pid:tid 唯一，32-status-toolbar.md §4.2）。
pub fn ctl_pipe_name(pid: u32, tid: u32) -> String {
    format!("{CTL_PIPE_PREFIX}-{pid}-{tid}")
}

/// daemon → TSF 的控制命令（按需连接 per-实例管道，32-status-toolbar.md §4.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlCmd {
    /// 设置某字段为指定值（field 0=mode 1=width 2=script 3=punct，value 0/1）。
    SetState { field: u8, value: u8 },
}

/// TSF 应用命令后的响应（§6.5 点击协议：daemon 按结果更新实例表 + 按钮图标）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CtlResult {
    /// 应用成功：返回**新**四态（成功后 TSF 还会 StateSync 上报，双路径一致）。
    Ok { state: ToolbarState },
    /// 应用失败（写 OPENCLOSE 失败/非法字段等）。
    Err { msg: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_state_field_accessor() {
        let s = ToolbarState::new(1, 0, 1, 0);
        assert_eq!(s.field(CTL_FIELD_MODE), 1);
        assert_eq!(s.field(CTL_FIELD_WIDTH), 0);
        assert_eq!(s.field(CTL_FIELD_SCRIPT), 1);
        assert_eq!(s.field(CTL_FIELD_PUNCT), 0);
        assert_eq!(s.field(0xFF), 0, "非法字段 → 0");
        assert_eq!(ctl_pipe_name(1234, 56), r"\\.\pipe\iuv-ctl-1234-56");
    }
}