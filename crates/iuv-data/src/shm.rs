//! 用户库共享内存段（M6 守护进程唯一写者 → 会话进程只读引用）。
//!
//! 设计见 `docs/plan/22-m6-daemon.md` §3。**M6 仅 Windows 生效**；非 Windows 编译降级
//! stub（全部方法返回 `Err(Unsupported)`，保持编译与跨平台测试可跑）。
//!
//! 段布局（`#[repr(C)]` 头 + 变长数据区，段容量固定 `SHM_CAPACITY`）：
//!
//! ```text
//! [0..8]    magic = b"IUVSHM01"
//! [8..12]   u32 data_len      —— 数据区已写字节数（序列化 UserDict 长度）
//! [12..16]  u32 version       —— 用户库纪元：守护进程每次写数据后递增（原子更新，
//!                                会话进程据此判定"需重解析段"）
//! [16..20]  u32 config_epoch  —— 配置纪元：设置页保存 config.json 后递增（原子更新，
//!                                会话进程据此判定"需重载配置"）
//! [20..20+D] 数据区           —— 序列化 UserDict 字节（IUVUSR02 线性格式，见
//!                                `UserDict::to_bytes`；data_len 决定实际长度）
//! ```
//!
//! 头为 20 字节：magic(8) + data_len(4) + version(4) + config_epoch(4)，四字段互不重叠、
//! 各自 4 字节对齐（u32 原子访问安全）。config_epoch 为 M6 追加（任务书 §5）——
//! 数据区随之后移，会话进程客户端以本文件注释布局为准。
//!
//! **写序与读序（跨进程数据竞）**：守护进程 `write()` 严格按
//! `数据区 → data_len(Release) → version(Release)` 写；会话进程 `read()` 先
//! `version(Acquire)` 判变，再 `data_len(Acquire)` 取长，最后拷数据。
//! Release/Acquire 配对保证：version 变化可见时，其之前的 data_len 与数据区写入
//! 也全部可见（seqlock 式先后写序）——读侧绝不见"半新半旧"。
//! version/config_epoch 字段经 `AtomicU32::from_ptr`（稳定 1.75+）跨进程原子访问
//! （x86 上映射到 lock xchg / mov，进程无关）。
//!
//! 视图生命周期：`MapViewOfFile` 建立的视图在 `UnmapViewOfFile` 前一直有效，
//! 映射句柄创建后即可关闭（视图自持引用）——故 ShmWriter/ShmReader 的 `Drop`
//! 只卸载视图，无需再关句柄（见 MSDN MapViewOfFile）。

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Memory::UnmapViewOfFile;

use crate::UserDict;

/// 段 magic。
const MAGIC: &[u8; 8] = b"IUVSHM01";
/// 头长度：magic(8) + data_len(4) + version(4) + config_epoch(4)。
const SHM_HEADER_LEN: usize = 20;
/// 段总容量（字节）。数据区 = 容量 − 头。用户库小（几千~几万条），4MB 充裕。
const SHM_CAPACITY: usize = 4 * 1024 * 1024;
/// 命名空间内共享对象名（`Local\` = 会话内隔离，单用户桌面足够；跨会话需 `Global\` 提权）。
const SHM_NAME: &str = "Local\\iuv-userdict-shm";

/// 数据区最大长度。
const DATA_CAPACITY: usize = SHM_CAPACITY - SHM_HEADER_LEN;

/// 数据区偏移（头后）。
const DATA_OFFSET: usize = SHM_HEADER_LEN;
/// data_len 字段偏移。
const DATA_LEN_OFFSET: usize = 8;
/// version 字段偏移。
const VERSION_OFFSET: usize = 12;
/// config_epoch 字段偏移。
const CONFIG_EPOCH_OFFSET: usize = 16;

fn err(kind: io::ErrorKind, msg: impl AsRef<str>) -> io::Error {
    io::Error::new(kind, msg.as_ref())
}

#[cfg(not(windows))]
fn err_unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "共享内存段 M6 仅 Windows 生效（iuv-data/src/shm.rs 非 Windows 为桩）",
    )
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, OpenFileMappingW, FILE_MAP_READ,
        FILE_MAP_WRITE, PAGE_READWRITE,
    };
    use windows_core::PCWSTR;

    fn name_wide() -> Vec<u16> {
        OsStr::new(SHM_NAME).encode_wide().chain(Some(0)).collect()
    }

    /// 打开现有段只读视图（会话进程用）。失败 = 守护进程未建段（降级信号）。
    /// 返回 `(映射句柄, 视图指针)`。**句柄须持有到 Unmap**——MSDN：映射对象在最后一个
    /// 句柄关闭时销毁，视图仅在本进程仍有效（跨进程/命名不可再寻）；句柄 = 命名段存活的凭据。
    pub fn open_read() -> io::Result<(HANDLE, *const u8)> {
        let wide = name_wide();
        // SAFETY: wide 以 NUL 结尾。
        let mapping = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(wide.as_ptr())) }
            .map_err(|e| {
                err(
                    io::ErrorKind::NotFound,
                    format!("打开共享段失败 {SHM_NAME}: {}", e.code()),
                )
            })?;
        // SAFETY: FILE_MAP_READ 映射整个段（0 = 到对象大小）。
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
        if view.Value.is_null() {
            // SAFETY: 映射失败，关闭句柄。
            let _ = unsafe { CloseHandle(mapping) };
            return Err(err(io::ErrorKind::Other, "映射共享段失败（只读）"));
        }
        Ok((mapping, view.Value as *const u8))
    }

    /// 创建/打开段可写视图（守护进程用；已存在则复用，双开同名字段 = 同段）。
    /// 返回 `(映射句柄, 视图指针, 是否新建)`；句柄须持有到 Unmap（同上注释）。
    pub fn create_or_open_write() -> io::Result<(HANDLE, *mut u8, bool)> {
        let wide = name_wide();
        // SAFETY: 无名文件映射（INVALID_HANDLE_VALUE）= 页文件后备，无需文件句柄；
        // PAGE_READWRITE 可写；容量 = SHM_CAPACITY（固定，首个创建者决定）。
        let mapping = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                SHM_CAPACITY as u32,
                PCWSTR(wide.as_ptr()),
            )
        }
        .map_err(|e| {
            err(
                io::ErrorKind::Other,
                format!("创建共享段失败 {SHM_NAME}: {}", e.code()),
            )
        })?;
        // SAFETY: GetLastError 紧接创建调用后取（无介入调用）。
        let created = unsafe { windows::Win32::Foundation::GetLastError() != ERROR_ALREADY_EXISTS };
        // SAFETY: FILE_MAP_WRITE 映射整个段。
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_WRITE, 0, 0, 0) };
        if view.Value.is_null() {
            // SAFETY: 映射失败，关闭句柄。
            let _ = unsafe { CloseHandle(mapping) };
            return Err(err(io::ErrorKind::Other, "映射共享段失败（可写）"));
        }
        Ok((mapping, view.Value as *mut u8, created))
    }
}

/// 共享段可写端（守护进程唯一写者）。非 Windows = 桩（不可构造成功）。
pub struct ShmWriter {
    #[cfg(windows)]
    mapping: HANDLE,
    #[cfg(windows)]
    view: *mut u8,
    /// 最近一次写入的 version（写前自增；打开时从段头续读，重启不重置纪元）。
    #[cfg(windows)]
    version: u32,
    #[cfg(windows)]
    config_epoch: u32,
    #[cfg(not(windows))]
    _stub: (),
}

// SAFETY: view/mapping 由本对象独占持有（生命周期 = 对象）；共享映射区域由调用方
// （DaemonState 内 Mutex）串行访问，读侧视图可跨线程只读访问——与 mmap.rs Mapped 同理由。
#[cfg(windows)]
unsafe impl Send for ShmWriter {}
#[cfg(windows)]
unsafe impl Sync for ShmWriter {}

impl ShmWriter {
    /// 创建/打开段（可写映射）。已存在段则复用（单实例守护进程场景 = 首建）。
    pub fn create_or_open() -> io::Result<ShmWriter> {
        #[cfg(windows)]
        {
            let (mapping, view, created) = imp::create_or_open_write()?;
            if created {
                // 新段：写入 magic 头（读侧据此判定段格式有效）。
                // SAFETY: view 指向可写映射（容量 SHM_CAPACITY ≥ 8）。
                unsafe {
                    std::ptr::copy_nonoverlapping(MAGIC.as_ptr(), view, MAGIC.len());
                }
                return Ok(ShmWriter {
                    mapping,
                    view,
                    version: 0,
                    config_epoch: 0,
                });
            }
            // 续读既有 version/config_epoch（守护重启不重置纪元，避免读侧抖动）。
            let base = view.cast::<u32>();
            // SAFETY: 段容量 SHM_CAPACITY ≥ 20，头四字段均在映射内且 4 字节对齐。
            let v = unsafe { AtomicU32::from_ptr(base.add(VERSION_OFFSET / 4)) };
            let ce = unsafe { AtomicU32::from_ptr(base.add(CONFIG_EPOCH_OFFSET / 4)) };
            Ok(ShmWriter {
                mapping,
                view,
                version: v.load(Ordering::Acquire),
                config_epoch: ce.load(Ordering::Acquire),
            })
        }
        #[cfg(not(windows))]
        {
            Err(err_unsupported())
        }
    }

    /// 写入用户库：to_bytes → memcpy 数据区 → data_len(Release) → version(Release)。
    /// 返回写后的 version（会话进程响应可携带，非必须）。
    pub fn write(&mut self, dict: &UserDict) -> io::Result<u32> {
        #[cfg(windows)]
        {
            let bytes = dict.to_bytes();
            if bytes.len() > DATA_CAPACITY {
                return Err(err(
                    io::ErrorKind::InvalidData,
                    format!("用户库序列化超段容量 {} > {}", bytes.len(), DATA_CAPACITY),
                ));
            }
            // SAFETY: view 指向可写映射（容量 SHM_CAPACITY）；DATA_OFFSET + bytes.len()
            // ≤ SHM_CAPACITY（已校验 bytes.len() ≤ DATA_CAPACITY）。
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    self.view.add(DATA_OFFSET),
                    bytes.len(),
                );
            }
            let v = self.version.wrapping_add(1);
            let base = self.view.cast::<u32>();
            // 先后写序：data_len 先（Release），version 最后（Release）。
            // 读侧 Acquire version 后，data_len 与数据区写入必可见（无半新半旧）。
            // SAFETY: 头四字段均在映射内且 4 字节对齐。
            unsafe {
                AtomicU32::from_ptr(base.add(DATA_LEN_OFFSET / 4))
                    .store(bytes.len() as u32, Ordering::Release);
                AtomicU32::from_ptr(base.add(VERSION_OFFSET / 4)).store(v, Ordering::Release);
            }
            self.version = v;
            Ok(v)
        }
        #[cfg(not(windows))]
        {
            let _ = dict;
            Err(err_unsupported())
        }
    }

    /// 递增并写 config_epoch（设置页保存后调用；会话进程检测变化重载 config）。
    pub fn bump_config_epoch(&mut self) -> u32 {
        #[cfg(windows)]
        {
            let e = self.config_epoch.wrapping_add(1);
            let base = self.view.cast::<u32>();
            // SAFETY: config_epoch 字段在映射内且 4 字节对齐。
            unsafe { AtomicU32::from_ptr(base.add(CONFIG_EPOCH_OFFSET / 4)).store(e, Ordering::Release) };
            self.config_epoch = e;
            e
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// 当前 version（最近一次写后的纪元；Ping 响应/日志用）。
    pub fn version(&self) -> u32 {
        #[cfg(windows)]
        {
            self.version
        }
        #[cfg(not(windows))]
        {
            0
        }
    }
}

impl Drop for ShmWriter {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: 视图与句柄由本对象独占持有；关闭失败无处理路径，忽略。
            let _ = unsafe {
                UnmapViewOfFile(windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.view as *mut _,
                })
            };
            let _ = unsafe { CloseHandle(self.mapping) };
        }
        #[cfg(not(windows))]
        {
            let _ = ();
        }
    }
}

/// 共享段只读端（会话进程）。非 Windows = 桩。
pub struct ShmReader {
    #[cfg(windows)]
    mapping: HANDLE,
    #[cfg(windows)]
    view: *const u8,
    #[cfg(not(windows))]
    _stub: (),
}

// SAFETY: 同 ShmWriter：视图由本对象独占持有，跨进程映射区域只读访问线程安全。
#[cfg(windows)]
unsafe impl Send for ShmReader {}
#[cfg(windows)]
unsafe impl Sync for ShmReader {}

impl ShmReader {
    /// 打开现有段只读映射。段不存在（守护进程未启动）→ `Err(NotFound)`，调用方降级。
    pub fn open() -> io::Result<ShmReader> {
        #[cfg(windows)]
        {
            let (mapping, view) = imp::open_read()?;
            Ok(ShmReader { mapping, view })
        }
        #[cfg(not(windows))]
        {
            Err(err_unsupported())
        }
    }

    /// 当前 version（原子读）。会话进程轮询：变化 → `read()` 重解析。
    pub fn version(&self) -> u32 {
        #[cfg(windows)]
        {
            let base = self.view.cast_mut().cast::<u32>();
            // SAFETY: version 字段在映射内且 4 字节对齐（只读原子取，指针可变性仅满足 from_ptr）。
            unsafe { AtomicU32::from_ptr(base.add(VERSION_OFFSET / 4)).load(Ordering::Acquire) }
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// 当前 config_epoch（原子读）。会话进程轮询：变化 → 重载 config.json。
    pub fn config_epoch(&self) -> u32 {
        #[cfg(windows)]
        {
            let base = self.view.cast_mut().cast::<u32>();
            // SAFETY: config_epoch 字段在映射内且 4 字节对齐（只读原子取，指针可变性仅满足 from_ptr）。
            unsafe {
                AtomicU32::from_ptr(base.add(CONFIG_EPOCH_OFFSET / 4)).load(Ordering::Acquire)
            }
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// 解析当前用户库。返回语义：
    /// - `Ok(Some(dict))`：段有效，解析成功；
    /// - `Ok(None)`：段存在但未写入（version=0 且 data_len=0）或 magic 不符（格式演进）——
    ///   调用方视同"暂无可读用户库"降级；
    /// - `Err`：data_len 越界 / 序列化损坏——调用方降级自读文件。
    pub fn read(&self) -> io::Result<Option<UserDict>> {
        #[cfg(windows)]
        {
            let base = self.view.cast_mut().cast::<u32>();
            // SAFETY: 头四字段均在映射内且 4 字节对齐（只读原子取，指针可变性仅满足 from_ptr）。
            let version =
                unsafe { AtomicU32::from_ptr(base.add(VERSION_OFFSET / 4)).load(Ordering::Acquire) };
            let data_len = unsafe {
                AtomicU32::from_ptr(base.add(DATA_LEN_OFFSET / 4)).load(Ordering::Acquire) as usize
            };
            // SAFETY: magic 在段头 [0..8]（段容量 SHM_CAPACITY ≥ 8）。
            let magic_bytes = unsafe { std::slice::from_raw_parts(self.view, 8) };
            if magic_bytes != MAGIC {
                return Ok(None); // 段存在但格式不符（不同代 IUVSHM 布局）
            }
            if version == 0 && data_len == 0 {
                return Ok(None); // 尚未首次写入
            }
            if data_len > DATA_CAPACITY {
                return Err(err(
                    io::ErrorKind::InvalidData,
                    format!("共享段 data_len 越界: {data_len}"),
                ));
            }
            // SAFETY: data_len ≤ DATA_CAPACITY，故 [DATA_OFFSET, DATA_OFFSET+data_len)
            // 在段内；version 已在 Acquire 读（其前写入全部可见）。
            let data = unsafe { std::slice::from_raw_parts(self.view.add(DATA_OFFSET), data_len) };
            match UserDict::from_bytes(data) {
                Ok(d) => Ok(Some(d)),
                Err(e) => Err(e),
            }
        }
        #[cfg(not(windows))]
        {
            Err(err_unsupported())
        }
    }
}

impl Drop for ShmReader {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: 视图与句柄由本对象独占持有；关闭失败无处理路径，忽略。
            let _ = unsafe {
                UnmapViewOfFile(windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.view as *mut _,
                })
            };
            let _ = unsafe { CloseHandle(self.mapping) };
        }
        #[cfg(not(windows))]
        {
            let _ = ();
        }
    }
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 共享段是进程级命名对象：三测试并行会互相覆盖，串行执行。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn shm_write_read_roundtrip() {
        let _guard = TEST_LOCK.lock().unwrap();
        // 写者 + 读者对同一段：写入后读者解析出的用户库与写前一致。
        let dict = UserDict::empty()
            .set_entry("da", "龘", 8000)
            .block("shan", "羴");
        let mut w = ShmWriter::create_or_open().unwrap();
        w.write(&dict).unwrap();
        let r = ShmReader::open().unwrap();
        let read = r.read().unwrap().expect("段已写入，应可解析");
        assert!(read
            .adjusted("da")
            .iter()
            .any(|(w_, a)| w_ == "龘" && *a == 8000));
        assert!(read.is_blocked("shan", "羴"));
    }

    #[test]
    fn shm_version_bumps_on_write() {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut w = ShmWriter::create_or_open().unwrap();
        let r = ShmReader::open().unwrap();
        let v0 = r.version();
        w.write(&UserDict::empty().set_entry("da", "龘", 1))
            .unwrap();
        assert_eq!(r.version(), v0.wrapping_add(1), "每次写入 version +1");
        w.write(&UserDict::empty().set_entry("ben", "犇", 2))
            .unwrap();
        assert_eq!(r.version(), v0.wrapping_add(2));
    }

    #[test]
    fn shm_config_epoch_bumps() {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut w = ShmWriter::create_or_open().unwrap();
        let r = ShmReader::open().unwrap();
        let e0 = r.config_epoch();
        let e1 = w.bump_config_epoch();
        assert_eq!(e1, e0.wrapping_add(1));
        assert_eq!(r.config_epoch(), e1);
    }
}
