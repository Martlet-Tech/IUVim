//! 管道消息类型（P3.2 自 iuv-data/ipc.rs 移入 iuv-win）：Request/Response + 工具栏四态
//! + 反向控制通道 Cmd/Result。编码见 `super::codec`，传输见 `super::pipe`/`super::ctl`。

use iuv_core::ImeState;

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
        state: ImeState,
    },
    /// 32-status-toolbar.md §4.1：实例运行时四态变化上报（OPENCLOSE OnChange /
    /// CtlCmd::Set* 应用成功后）。
    StateSync {
        pid: u32,
        tid: u32,
        state: ImeState,
    },
    /// 32-status-toolbar.md §4.1：Activate/Deactivate 通知（daemon 判「iuv 被选中」）。
    Active { pid: u32, tid: u32, active: bool },
    /// 32-status-toolbar.md §4.1：语言栏右键菜单「显示/隐藏工具栏」（全局偏好切换）。
    ToggleToolbar,
    /// 32-status-toolbar.md §4.1：实例 Drop 注销（从实例表移除）。
    Unregister { pid: u32, tid: u32 },
    /// 语言栏右键菜单打开时查询工具栏全局显隐偏好（菜单项文案「显示/隐藏工具栏」二选一）。
    GetToolbarVisible,
}

/// 守护进程 → 会话进程的响应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Ok { version: u32 },
    Err { msg: String },
    /// `Request::GetToolbarVisible` 的应答：当前全局显隐偏好。
    ToolbarVisible { visible: bool },
}

/// 工具条信号通道载荷（40-toolbar-show-hide-governance.md 纯信号模型定稿）：
/// 专用管道 `iuv-toolbar-signal`，与数据面（用户库写）物理隔离。
/// 三消息 = 显隐决策唯一输入；TSF 实例获得焦点发 FocusGained、失去焦点发
/// FocusLost、运行中四态变化发 StateChanged。pid/tid 仅作日志观察点。
///
/// M1 桌宠骨架扩展：新增 `Typing`（tag `0x24`）——组合开始（内容非空）→ `active=true`、
/// 组合结束/提交/取消 → `active=false`，供 daemon 驱动宠物"打字敲键盘"动画。
/// "打字"作为**事件**而非**状态**——不并入 ImeState，独立信号通道（避免污染
/// 32-toolbar §5.1 的"实例运行时值"语义）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolbarSignal {
    /// 激活：「有一个实例持有者获得了焦点」+ 当前四态（供 daemon 渲染新工具栏）。
    FocusGained { pid: u32, tid: u32, state: ImeState },
    /// 失焦：「有一个实例持有者宣布了自己失焦」。
    FocusLost { pid: u32, tid: u32 },
    /// 态变更：会话中途四态变化（工具栏按钮/系统级切换后实例自报新态）。
    StateChanged { pid: u32, tid: u32, state: ImeState },
    /// 打字中：组合开始（active=true）/ 结束-提交-取消（active=false）。
    /// M1 桌宠专用，daemon 据此驱动宠物"敲键盘律动"动画 + 空闲停帧回退。
    Typing { pid: u32, tid: u32, active: bool },
}

// ===== 32-status-toolbar.md 工具栏四态 + 反向控制通道 =====

// 四态在消息里直接用 iuv-core `ImeState`（全仓唯一表示；线编码 = `[u8;4]`，
// 转换点在 iuv-core runtime.rs，codec.rs 编解码时套用）。

/// 反向控制通道管道名前缀：`\\.\pipe\iuv-ctl-<pid>-<tid>`。
const CTL_PIPE_PREFIX: &str = r"\\.\pipe\iuv-ctl";

/// 实例控制管道完整名（pid:tid 唯一，32-status-toolbar.md §4.2）。
pub fn ctl_pipe_name(pid: u32, tid: u32) -> String {
    format!("{CTL_PIPE_PREFIX}-{pid}-{tid}")
}

/// daemon → TSF 的控制命令（按需连接 per-实例管道，32-status-toolbar.md §4.2）。
/// 每字段一变体（无线字段序数协议）：true = 切到第二态（英/全/繁/英标），false = 第一态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlCmd {
    /// 中/英（mode 走 OPENCLOSE compartment 真相源，TSF 侧特殊处理）。
    SetMode(bool),
    /// 半角/全角。
    SetWidth(bool),
    /// 简体/繁体。
    SetScript(bool),
    /// 中文标点/英文标点。
    SetPunct(bool),
}

/// TSF 应用命令后的响应（§6.5 点击协议：daemon 按结果更新实例表 + 按钮图标）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CtlResult {
    /// 应用成功：返回**新**四态（成功后 TSF 还会 StateSync 上报，双路径一致）。
    Ok { state: ImeState },
    /// 应用失败（写 OPENCLOSE 失败等）。
    Err { msg: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctl_pipe_name_format() {
        assert_eq!(ctl_pipe_name(1234, 56), r"\\.\pipe\iuv-ctl-1234-56");
    }
}