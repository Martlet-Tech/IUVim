//! 守护进程共享状态 + 用户库发布链路（写共享段 → bump version → 聚合写盘）。
//!
//! 所有写用户库的入口（管道线程、设置页「清除全部」）统一走 `publish()`：
//! 锁 dict → 序列化 → 写共享段（ShmWriter::write 内部按"数据区 → data_len → version"
//! 顺序原子发布）→ 置 dirty。`flush_if_dirty()` 由主线程 2s 定时器调用，聚合写盘。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use iuv_data::{ShmWriter, UserDict};

use crate::config::DaemonConfig;
use crate::log;

/// 守护进程全局状态（Arc 分发给主线程 / 管道线程 / 设置窗口线程）。
pub struct DaemonState {
    /// 用户库内存态（唯一写者 = 守护进程；写时复制替换）。
    pub dict: Mutex<UserDict>,
    /// 共享段写者（None = 建段失败，发布仅置 dirty，仍可写盘）。
    pub shm: Mutex<Option<ShmWriter>>,
    /// 待写盘标记（管道写请求/设置页改动置位；主线程 2s 定时器消费）。
    pub dirty: AtomicBool,
    /// 设置窗口的 egui Context（窗口线程注册；主线程据此 Close/重绘）。
    pub settings_ctx: Mutex<Option<egui::Context>>,
    /// 退出信号：主线程置位 → 设置线程收到后关闭窗口。
    pub close_settings: AtomicBool,
    /// 当前配置快照（设置页保存后更新；托盘菜单主题读取）。
    pub config: Mutex<DaemonConfig>,
    /// 用户库文件路径（写盘目标）。
    pub user_dict_path: PathBuf,
}

impl DaemonState {
    pub fn new(
        dict: UserDict,
        shm: Option<ShmWriter>,
        config: DaemonConfig,
        user_dict_path: PathBuf,
    ) -> Arc<Self> {
        Arc::new(DaemonState {
            dict: Mutex::new(dict),
            shm: Mutex::new(shm),
            dirty: AtomicBool::new(false),
            settings_ctx: Mutex::new(None),
            close_settings: AtomicBool::new(false),
            config: Mutex::new(config),
            user_dict_path,
        })
    }

    /// 发布用户库：dict（锁）→ 写共享段（bump version）→ 置 dirty。
    /// 返回共享段新 version（管道 Response::Ok 携带）；shm 缺失 → None。
    pub fn publish(&self) -> Option<u32> {
        let dict = self.dict.lock().unwrap_or_else(|p| p.into_inner());
        let mut shm = self.shm.lock().unwrap_or_else(|p| p.into_inner());
        let v = match shm.as_mut() {
            Some(w) => match w.write(&dict) {
                Ok(v) => {
                    log::log_line(&format!("[state] 已发布用户库到共享段 version={v}"));
                    Some(v)
                }
                Err(e) => {
                    log::log_line(&format!("[state] 写共享段失败（内存态仍生效）: {e}"));
                    None
                }
            },
            None => None,
        };
        self.dirty.store(true, Ordering::Release);
        v
    }

    /// 当前共享段 version（Ping 响应用）；shm 缺失 → 0。
    pub fn current_version(&self) -> u32 {
        let shm = self.shm.lock().unwrap_or_else(|p| p.into_inner());
        shm.as_ref().map(|w| w.version()).unwrap_or(0)
    }

    /// 递增共享段 config_epoch（设置页保存后调用；会话进程检测变化重载 config）。
    pub fn bump_config_epoch(&self) -> u32 {
        let mut shm = self.shm.lock().unwrap_or_else(|p| p.into_inner());
        match shm.as_mut() {
            Some(w) => {
                let e = w.bump_config_epoch();
                log::log_line(&format!("[state] config_epoch 递增 → {e}"));
                e
            }
            None => {
                log::log_line("[state] config_epoch 递增跳过（共享段缺失）");
                0
            }
        }
    }

    /// 2s 聚合写盘（主线程定时器回调）：dirty → 用户库写文件。失败保留 dirty 重试。
    pub fn flush_if_dirty(&self) {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let dict = self.dict.lock().unwrap_or_else(|p| p.into_inner());
        match dict.save(&self.user_dict_path) {
            Ok(()) => {
                log::log_line(&format!("[state] 用户库已写盘：{}", self.user_dict_path.display()))
            }
            Err(e) => {
                log::log_line(&format!(
                    "[state] 写盘失败（保留 dirty，下轮重试）: {e}"
                ));
                self.dirty.store(true, Ordering::Release);
            }
        }
    }

    /// 退出前强写盘（dirty 未消费也写）。
    pub fn flush_now(&self) {
        self.flush_if_dirty();
        // 若首次写盘失败，最多再补 3 次（间隔 50ms）。
        for _ in 0..3 {
            if !self.dirty.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.flush_if_dirty();
        }
    }
}