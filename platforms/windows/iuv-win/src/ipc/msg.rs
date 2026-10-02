//! 旧管道消息类型残余（49 号 ②/③ 迁移后的存活集）。
//!
//! 数据面 Request（用户库写/注册/信号等 12 变体）与 Response 已随旧管道退役——
//! 数据面走 transport `C2S::UserMutation`，工具栏信号走 transport `C2S::*` 直达；
//! 反向控制走 transport `S2C::Ctl`。此处仅存：
//!
//! - `Request::ToggleToolbar`：语言栏菜单「显示/隐藏工具栏」——server 进程内直调
//!   `ToolbarHost::handle_request`（无线上形态）；
//! - `ToolbarSignal`：工具栏显隐/四态/打字信号类型（40 号纯信号模型），server 内
//!   由 `EngineSession` C2S 路由构造、daemon 工具栏宿主消费（进程内直通）；
//! - `CtlCmd`/`CtlResult`：③-2 沉底 iuv-data 的全仓唯一定义，此处 re-export
//!   维持 `iuv_win::` 路径（transport 控制面线上载荷）。

use iuv_core::ImeState;

/// 工具条宿主命令（仅存语言栏菜单开关；其余 Request 变体已随旧管道退役）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// 32-status-toolbar.md §4.1：语言栏右键菜单「显示/隐藏工具栏」（全局偏好切换）。
    ToggleToolbar,
}

/// 工具条信号（40-toolbar-show-hide-governance.md 纯信号模型定稿）。
/// 三消息 = 显隐决策唯一输入；TSF 实例获得焦点发 FocusGained、失去焦点发
/// FocusLost、运行中四态变化发 StateChanged。pid/tid 仅作日志观察点。
///
/// M1 桌宠骨架扩展：新增 `Typing`——组合开始（内容非空）→ `active=true`、
/// 组合结束/提交/取消 → `active=false`，供 daemon 驱动宠物"打字敲键盘"动画。
/// "打字"作为**事件**而非**状态**——不并入 ImeState，独立信号通道（避免污染
/// 32-toolbar §5.1 的"实例运行时值"语义）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolbarSignal {
    /// 激活：「有一个实例持有者获得了焦点」+ 当前四态（供 daemon 渲染新工具栏）。
    FocusGained { pid: u32, tid: u32, state: ImeState },
    /// 失焦：「有一个实例持有者宣布了自己失焦」。
    FocusLost { pid: u32, tid: u32 },
    /// 实例停用：TSF Deactivate / 实例 Drop——iuv 被整体切走或卸载，强于失焦
    /// （解绑不受设置窗失焦粘性抑制）。
    Deactivated { pid: u32, tid: u32 },
    /// 态变更：会话中途四态变化（工具栏按钮/系统级切换后实例自报新态）。
    StateChanged { pid: u32, tid: u32, state: ImeState },
    /// 打字中：组合开始（active=true）/ 结束-提交-取消（active=false）。
    /// M1 桌宠专用，daemon 据此驱动宠物"敲键盘律动"动画 + 空闲停帧回退。
    Typing { pid: u32, tid: u32, active: bool },
}

// ③-2 归一：CtlCmd/CtlResult 沉底 iuv-data 全仓唯一定义（bool 语义注释见其定义处），
// 此处唯 re-export 维持 `iuv_win::` 路径。
pub use iuv_data::{CtlCmd, CtlResult};
