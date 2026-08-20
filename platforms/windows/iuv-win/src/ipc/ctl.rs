//! 反向控制通道（P3.2 自 iuv-data/ipc.rs 移入 iuv-win）：daemon → 每 TSF 实例的
//! 控制管道（§4.2，按需连接 per-实例）。
//!
//! `CtlServer`（TSF 每实例一个 accept 线程）：阻塞等待 daemon 连接 → 处理一条
//! CtlCmd → 回 CtlResult → 断开。句柄经 `handle()` 暴露供调用方跨线程 `Close` 中断等待。
//!
//! `CtlClient`（daemon 侧）：连入 `\\.\pipe\iuv-ctl-<pid>-<tid>` → 发一帧 Cmd → 收一帧
//! CtlResult → 断开。

use std::io;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Pipes::DisconnectNamedPipe;

use super::codec::{
    decode_ctl_cmd, decode_ctl_result, encode_ctl_cmd, encode_ctl_result,
};
use super::msg::{CtlCmd, CtlResult};
use super::pipe::imp;

/// 反向控制管道服务端（TSF 每实例一个 accept 线程，§4.2）。句柄经 `handle()` 暴露供
/// 调用方跨线程 `Close` 中断等待。
pub struct CtlServer {
    pub(crate) handle: HANDLE,
}

impl CtlServer {
    /// 创建管道实例（**不连接**）：`connect` 才阻塞等待。句柄可由其他线程
    /// `CloseHandle` 中断 `connect`（TSF Deactivate 停 accept 线程用）。
    pub fn create(name: &str) -> io::Result<CtlServer> {
        let wide = imp::name_wide(name);
        let handle = imp::create_server(&wide)?;
        Ok(CtlServer { handle })
    }

    /// 阻塞等待 daemon 连接。跨线程 `CloseHandle` 中断 → `Err`（取消路径）。
    pub fn connect(&self) -> io::Result<()> {
        imp::connect_server(self.handle)
    }

    /// 读一 Cmd → `handler` 求结果 → 写回。任何一步失败返回 `Err`。
    pub fn serve(&self, handler: impl FnOnce(&CtlCmd) -> CtlResult) -> io::Result<()> {
        let payload = imp::read_frame(self.handle)?;
        let cmd = decode_ctl_cmd(&payload)?;
        let result = handler(&cmd);
        imp::write_frame(self.handle, &encode_ctl_result(&result))
    }

    /// 服务端句柄（跨进程/线程共享：调用方可持有后从其他线程 `CloseHandle` 中断阻塞等待）。
    pub fn handle(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for CtlServer {
    fn drop(&mut self) {
        // SAFETY: 断开 + 关闭句柄；重复关闭返回错误，忽略。
        let _ = unsafe { DisconnectNamedPipe(self.handle) };
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// 反向控制管道客户端（daemon 侧，§4.2 按需连接）。
pub struct CtlClient {
    handle: HANDLE,
}

impl CtlClient {
    /// 连接指定实例控制管道。实例不在线（未启动 accept / 已死）→ `Err`（daemon 干净退出）。
    pub fn connect(name: &str) -> io::Result<CtlClient> {
        let wide = imp::name_wide(name);
        let handle = imp::connect_client(&wide)?;
        Ok(CtlClient { handle })
    }

    /// 发命令 → 收结果（单次会话；成功后调用方断开连接，贴合按需连接风格）。
    pub fn request(&self, cmd: &CtlCmd) -> io::Result<CtlResult> {
        let _ = imp::write_frame(self.handle, &encode_ctl_cmd(cmd))?;
        let resp_payload = imp::read_frame(self.handle)?;
        decode_ctl_result(&resp_payload)
    }
}

impl Drop for CtlClient {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占持有；关闭失败无处理路径，忽略。
        let _ = unsafe { CloseHandle(self.handle) };
    }
}