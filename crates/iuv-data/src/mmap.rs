//! 词库文件的零拷贝视图（IMEDIC02 平面格式支撑）。
//!
//! Windows：`CreateFileW` + `CreateFileMappingW` + `MapViewOfFile`（读共享 + 延迟删除），
//! 物理内存全系统一份（页缓存），加载零复制零加工。声明 `FILE_SHARE_READ|WRITE|DELETE`
//! 使词库被热替换（dev-deploy 改名替换）时旧映射继续有效。
//! 非 Windows：退化为 `fs::read` 一次性读入（`Arc<[u8]>`），接口同形，跨平台测试可用。
//!
//! `as_bytes` 返回的视图在 `MappedFile` 存活期间有效；查询层只在该存活期内使用。

use std::io;
use std::path::Path;
use std::sync::Arc;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileSizeEx, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
    };
    use windows_core::PCWSTR;

    /// Windows 内存映射文件（RAII：Drop 依次 Unmap → Close(mapping) → Close(file)）。
    pub struct Mapped {
        file: HANDLE,
        mapping: HANDLE,
        view: *const u8,
        len: usize,
    }

    unsafe impl Send for Mapped {}
    unsafe impl Sync for Mapped {}

    fn err(kind: io::ErrorKind, msg: impl AsRef<str>) -> io::Error {
        io::Error::new(kind, msg.as_ref())
    }

    impl Mapped {
        pub fn open(path: &Path) -> io::Result<Mapped> {
            let wide: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();
            // SAFETY: wide 以 NUL 结尾；句柄由调用方负责关闭。
            let file = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    GENERIC_READ.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    Default::default(),
                    None,
                )
            }
            .map_err(|e| {
                err(
                    io::ErrorKind::NotFound,
                    format!("打开词库失败 {}: {}", path.display(), e.code()),
                )
            })?;
            let result = Self::map(file, path);
            if result.is_err() {
                // SAFETY: 映射失败，关闭文件句柄（关闭失败无处理路径，忽略）。
                let _ = unsafe { CloseHandle(file) };
            }
            result
        }

        fn map(file: HANDLE, path: &Path) -> io::Result<Mapped> {
            // 文件大小（映射视图只覆盖文件范围，杜绝访问文件尾后对齐页）。
            // SAFETY: file 句柄有效。
            let mut size: i64 = 0;
            unsafe { GetFileSizeEx(file, &mut size) }
                .map_err(|e| err(io::ErrorKind::Other, format!("查询词库大小失败 {}: {}", path.display(), e.code())))?;
            let len = size as usize;
            if len == 0 {
                return Err(err(io::ErrorKind::InvalidData, format!("词库为空: {}", path.display())));
            }
            // SAFETY: 只读映射整个文件；句柄由本对象持有。
            let mapping = unsafe { CreateFileMappingW(file, None, PAGE_READONLY, 0, 0, PCWSTR::null()) }
                .map_err(|e| err(io::ErrorKind::Other, format!("创建映射失败 {}: {}", path.display(), e.code())))?;
            let result = (|| {
                // SAFETY: 只读映射整个文件（0 = 到文件尾）。
                let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
                if view.Value.is_null() {
                    return Err(err(io::ErrorKind::Other, format!("映射视图失败: {}", path.display())));
                }
                Ok(Mapped { file, mapping, view: view.Value as *const u8, len })
            })();
            if result.is_err() {
                // SAFETY: 映射视图失败，关闭映射句柄。
                let _ = unsafe { CloseHandle(mapping) };
            }
            result
        }

        pub fn as_bytes(&self) -> &[u8] {
            // SAFETY: view 由本对象持有，存活期间有效；len = 文件大小（映射覆盖全文件）。
            unsafe { std::slice::from_raw_parts(self.view, self.len) }
        }
    }

    impl Drop for Mapped {
        fn drop(&mut self) {
            // SAFETY: 句柄/视图由本对象独占持有；关闭失败无处理路径，忽略。
            let _ = unsafe {
                UnmapViewOfFile(windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.view as *mut _,
                })
            };
            let _ = unsafe { CloseHandle(self.mapping) };
            let _ = unsafe { CloseHandle(self.file) };
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::fs;

    /// 非 Windows：整文件读入内存（Arc 共享，行为与 mmap 同形）。
    pub struct Mapped {
        data: Arc<[u8]>,
    }

    impl Mapped {
        pub fn open(path: &Path) -> io::Result<Mapped> {
            let data = fs::read(path)?;
            Ok(Mapped { data: data.into() })
        }

        pub fn as_bytes(&self) -> &[u8] {
            &self.data
        }
    }
}

/// 词库字节视图统一入口：Windows mmap / 非 Windows 整读 / 内存构造（测试与 from_entries）。
pub struct MappedFile {
    repr: Repr,
}

impl std::fmt::Debug for MappedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedFile")
            .field("len", &self.as_bytes().len())
            .finish()
    }
}

enum Repr {
    Mapped(imp::Mapped),
    Heap(Arc<[u8]>),
}

impl MappedFile {
    pub fn open(path: &Path) -> io::Result<MappedFile> {
        Ok(MappedFile { repr: Repr::Mapped(imp::Mapped::open(path)?) })
    }

    /// 内存字节构造（from_entries 统一路径与测试用）。
    pub fn from_vec(data: Vec<u8>) -> MappedFile {
        MappedFile { repr: Repr::Heap(data.into()) }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match &self.repr {
            Repr::Mapped(m) => m.as_bytes(),
            Repr::Heap(a) => a,
        }
    }

}
