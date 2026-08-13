//! 二进制词典格式读写（magic = `IMEDIC01`）。契约 01-contract.md §3.1。
//!
//! 布局：
//! ```text
//! [0..8]    magic = b"IMEDIC01"
//! [8..12]   u32 LE  record_count
//! 记录×N:   u8 code_len | code（squashed，全小写 a-z，无空格）
//!           u16 LE word_utf8_len | word（UTF-8）
//!           u32 LE weight
//! ```
//! 记录按 (code 升序, weight 降序) 排列写入；加载时经 `Dict::from_entries` 建表
//! （其内部会再排序去重，天然防手写数据不规范）。

use crate::{Dict, Entry};
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// 文件头 magic（含版本号；格式演进升 `IMEDIC02` 并做向后兼容读取）。
const MAGIC: &[u8; 8] = b"IMEDIC01";

/// 流式游标：自带偏移统计，越界统一报 `InvalidData`（带偏移），由调用方补文件名。
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize, what: &str) -> io::Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("偏移 {}: 读取{what}时文件截断", self.pos),
            ));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1, "字节")?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2, "u16")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4, "u32")?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// 从二进制文件加载词典。magic 校验失败 / 截断 / 坏数据 → `io::ErrorKind::InvalidData`，
/// 错误消息带文件名与偏移。
pub fn load(path: &Path) -> io::Result<Dict> {
    let data =
        fs::read(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    let mut c = Cursor {
        data: &data,
        pos: 0,
    };
    let magic = c.take(MAGIC.len(), "magic")?;
    if magic != MAGIC {
        return Err(bad(
            path,
            0,
            format!("magic 校验失败（期望 {MAGIC:?}，得到 {magic:?}）"),
        ));
    }
    let count = c.u32()? as usize;
    let mut items = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let code_len = c.u8()? as usize;
        let code_start = c.pos;
        let code = c.take(code_len, "code")?;
        let code = std::str::from_utf8(code).map_err(|_| bad(path, code_start, "code 非 UTF-8"))?;
        if !is_valid_code(code) {
            return Err(bad(path, code_start, format!("code 含非法字符: {code:?}")));
        }
        let word_len = c.u16()? as usize;
        let word_start = c.pos;
        let word_b = c.take(word_len, "word")?;
        let word =
            std::str::from_utf8(word_b).map_err(|_| bad(path, word_start, "word 非 UTF-8"))?;
        let weight = c.u32()?;
        items.push((code.to_string(), word.to_string(), weight));
    }
    if c.pos != data.len() {
        return Err(bad(path, c.pos, "文件尾部有多余数据"));
    }
    Ok(Dict::from_entries(items))
}

/// 写入二进制词典。records 按 (code 升序, weight 降序) 排列。
pub fn write(records: &[Entry], writer: impl io::Write) -> io::Result<()> {
    let mut w = BufWriter::new(writer);
    w.write_all(MAGIC)?;
    let count = u32::try_from(records.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "记录数超过 u32 上限"))?;
    w.write_all(&count.to_le_bytes())?;
    for r in records {
        let code = r.code.as_bytes();
        let code_len = u8::try_from(code.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "code 超过 255 字节"))?;
        w.write_all(&[code_len])?;
        w.write_all(code)?;
        let word = r.word.as_bytes();
        let word_len = u16::try_from(word.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "word 超过 65535 字节"))?;
        w.write_all(&word_len.to_le_bytes())?;
        w.write_all(word)?;
        w.write_all(&r.weight.to_le_bytes())?;
    }
    w.flush()
}

/// code 合法性：squashed 全小写 a-z，允许 `'` 强制分隔（如 `xi'an`）。
fn is_valid_code(code: &str) -> bool {
    code.chars().all(|c| c.is_ascii_lowercase() || c == '\'')
}

fn bad(path: &Path, offset: usize, msg: impl AsRef<str>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: 偏移 {offset}: {}", path.display(), msg.as_ref()),
    )
}
