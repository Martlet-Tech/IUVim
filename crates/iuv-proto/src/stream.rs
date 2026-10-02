//! stream_id 分配（49 §4.2）：客户端偶数 / 服务端奇数，单调递增，回绕跳过在途号。
//!
//! 在途并发实测个位数（热路径 1、控制面 ≤1），32768 号段余量充足；
//! 分配器仍按「无号可用返回 `None`」实现，不做 panic 兜底。

use std::collections::HashSet;

/// 单侧 stream_id 分配器。不共享、可移动（连接所有权模型，49 §4.7#8）；
/// Clone 供连接发送器多副本（ConnSender::clone，②控制面）。
#[derive(Debug, Clone)]
pub struct StreamIdAlloc {
    next: u16,
    step: u16,
    in_flight: HashSet<u16>,
}

impl StreamIdAlloc {
    /// 客户端侧：偶数号段（0, 2, 4, …）。
    pub fn client() -> StreamIdAlloc {
        StreamIdAlloc {
            next: 0,
            step: 2,
            in_flight: HashSet::new(),
        }
    }

    /// 服务端侧：奇数号段（1, 3, 5, …）。
    pub fn server() -> StreamIdAlloc {
        StreamIdAlloc {
            next: 1,
            step: 2,
            in_flight: HashSet::new(),
        }
    }

    /// 指定起点构造（确定性测试 / repl 复现用）。`odd` 仅作语义标注（T1 起步长
    /// 恒为 2，段归属由 `next` 自身奇偶决定，保留参数以免调用方改签名）。
    pub fn with_start(_odd: bool, next: u16) -> StreamIdAlloc {
        StreamIdAlloc {
            next,
            step: 2,
            in_flight: HashSet::new(),
        }
    }

    /// 分配下一个空闲号。号段耗尽（极端：全部 32768 个号在途）→ `None`。
    pub fn alloc(&mut self) -> Option<u16> {
        if self.in_flight.len() >= 32768 {
            return None;
        }
        loop {
            let id = self.next;
            self.next = self.next.wrapping_add(self.step);
            if self.in_flight.insert(id) {
                return Some(id);
            }
        }
    }

    /// 标记请求完成，回收号。
    pub fn release(&mut self, id: u16) {
        self.in_flight.remove(&id);
    }

    /// 在途请求数（观测/测试用）。
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}
