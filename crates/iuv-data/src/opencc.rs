//! 简→繁转换表（OpenCC s2t 数据）：IUVOCC01 二进制格式 + 加载 + 转换。
//!
//! 设计见 `docs/plan/31-script-traditional.md`：形态3 = 数据文件（与 iuv.imedic 同构），
//! 不编进 DLL、不经 daemon IPC。数据源 = OpenCC（Apache-2.0）`STPhrases.txt`/`STCharacters.txt`，
//! 由 dictc 编译成紧凑二进制；运行时解析成 HashMap（转换需随机查表，不做 mmap 零拷贝）。
//!
//! ## IUVOCC01 格式（简单线性，小文件）
//!
//! ```text
//! [0..8]   magic = b"IUVOCC01"
//! [8..12]  u32 LE phrase_count
//! [12..]   phrases: count × { u16 key_len | key | u16 val_len | val }
//! [..]     u32 LE char_count
//! [..]     chars:   count × { u16 key_len | key | u16 val_len | val }
//! ```
//!
//! - 键 = 简体，值 = 繁体（编译时取 OpenCC 多值首值，无上下文模型）。
//! - 段分离：短语表（多字）优先最长匹配，单字表兜底（OpenCC 语义同构）。
//! - 键/值均 UTF-8，长度 u16（OpenCC 数据均远小于 64KB）。

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

/// 文件头 magic。
const MAGIC: &[u8; 8] = b"IUVOCC01";

/// 简→繁转换表（编译产物解析结果，只读共享）。
#[derive(Clone, Debug, Default)]
pub struct OpenccTable {
    /// 多字短语表：简体短语 → 繁体（OpenCC STPhrases，取首值）。
    phrases: HashMap<String, String>,
    /// 单字表：简体字 → 繁体（OpenCC STCharacters，取首值）。
    chars: HashMap<char, String>,
    /// 短语表最长键的字符数（转换时最长匹配上界）。
    max_phrase_chars: usize,
}

impl OpenccTable {
    /// 空表（未装配/降级；转换返回原文）。
    pub fn empty() -> OpenccTable {
        OpenccTable::default()
    }

    /// 从文件加载 IUVOCC01。缺失/损坏 → `Err`（调用方决定降级为空表，不阻断引擎）。
    pub fn load(path: &Path) -> io::Result<OpenccTable> {
        let data = std::fs::read(path)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        Self::from_bytes(&data)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    }

    /// 从 IUVOCC01 二进制解析。magic/长度校验失败 → `Err(InvalidData)`。
    pub fn from_bytes(data: &[u8]) -> io::Result<OpenccTable> {
        let bad = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());
        if data.len() < 8 || &data[..8] != MAGIC {
            return Err(bad("magic 校验失败"));
        }
        let mut pos = 8;
        let phrase_count = read_u32(data, &mut pos, "短语数")? as usize;
        let mut phrases: HashMap<String, String> = HashMap::with_capacity(phrase_count);
        for _ in 0..phrase_count {
            let key = read_str(data, &mut pos, "短语键")?;
            let val = read_str(data, &mut pos, "短语值")?;
            phrases.insert(key, val);
        }
        let char_count = read_u32(data, &mut pos, "单字数")? as usize;
        let mut chars: HashMap<char, String> = HashMap::with_capacity(char_count);
        for _ in 0..char_count {
            let key = read_str(data, &mut pos, "单字键")?;
            let val = read_str(data, &mut pos, "单字值")?;
            let mut it = key.chars();
            let c = it.next().ok_or_else(|| bad("单字键为空"))?;
            if it.next().is_some() {
                return Err(bad("单字键非单字"));
            }
            chars.insert(c, val);
        }
        if pos != data.len() {
            return Err(bad("载荷尾部有残留字节"));
        }
        Ok(Self::from_maps(phrases, chars))
    }

    /// 由两个映射构建（归一化 max_phrase_chars）。
    fn from_maps(phrases: HashMap<String, String>, chars: HashMap<char, String>) -> OpenccTable {
        let max_phrase_chars = phrases.keys().map(|k| k.chars().count()).max().unwrap_or(0);
        OpenccTable {
            phrases,
            chars,
            max_phrase_chars,
        }
    }

    /// 序列化为 IUVOCC01 字节（dictc 编译产物 = 本函数输出）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64 + (self.phrases.len() + self.chars.len()) * 32);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&(self.phrases.len() as u32).to_le_bytes());
        for (k, v) in &self.phrases {
            put_str(&mut buf, k);
            put_str(&mut buf, v);
        }
        buf.extend_from_slice(&(self.chars.len() as u32).to_le_bytes());
        for (c, v) in &self.chars {
            let s: String = c.to_string();
            put_str(&mut buf, &s);
            put_str(&mut buf, v);
        }
        buf
    }

    /// 词条数（phrases + chars，日志用）。
    pub fn entry_count(&self) -> usize {
        self.phrases.len() + self.chars.len()
    }

    /// 简→繁转换：**正向最长匹配**。逐字符扫描：
    /// 1. 当前位置先试短语表最长键（短语优先，避免单字把整词拆散）；
    /// 2. 未命中短语则单字表兜底；
    /// 3. 两者未命中原样保留（汉字/拼音/符号/全角均直通，幂等——已繁体字符不二次转换）。
    pub fn convert(&self, text: &str) -> String {
        if self.phrases.is_empty() && self.chars.is_empty() {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0usize;
        while i < chars.len() {
            // 短语最长匹配：从 max_phrase_chars 向下试（下界 2，单字走 chars）。
            let mut matched = false;
            let hi = self.max_phrase_chars.min(chars.len() - i);
            for len in (2..=hi).rev() {
                let key: String = chars[i..i + len].iter().collect();
                if let Some(val) = self.phrases.get(&key) {
                    out.push_str(val);
                    i += len;
                    matched = true;
                    break;
                }
            }
            if matched {
                continue;
            }
            // 单字兜底
            if let Some(val) = self.chars.get(&chars[i]) {
                out.push_str(val);
            } else {
                out.push(chars[i]);
            }
            i += 1;
        }
        out
    }
}

/// 从 OpenCC 文本源构建转换表（dictc 编译：解析两个 txt → OpenccTable → to_bytes）。
/// 语法：`key\tvalue1 value2 ...`；`#` 注释行/空行跳过；容忍 BOM 与 CRLF；多值取首值。
/// 键为单字 → chars 段，多字 → phrases 段。
pub fn from_text(phrases_text: &str, chars_text: &str) -> io::Result<OpenccTable> {
    let mut phrases: HashMap<String, String> = HashMap::new();
    let mut chars: HashMap<char, String> = HashMap::new();
    let parse = |src: &str, multi: &mut HashMap<String, String>, single: &mut HashMap<char, String>| {
        for line in src.lines() {
            let line = line.strip_prefix('\u{feff}').unwrap_or(line);
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, '\t');
            let (key, vals) = match (parts.next(), parts.next()) {
                (Some(k), Some(v)) => (k.trim(), v.trim()),
                _ => continue,
            };
            if key.is_empty() || vals.is_empty() {
                continue;
            }
            let Some(val) = vals.split_whitespace().next().map(str::to_owned) else {
                continue;
            };
            if key.chars().count() == 1 {
                if let Some(c) = key.chars().next() {
                    single.insert(c, val);
                }
            } else {
                multi.insert(key.to_owned(), val);
            }
        }
    };
    parse(phrases_text, &mut phrases, &mut chars);
    parse(chars_text, &mut phrases, &mut chars);
    Ok(OpenccTable::from_maps(phrases, chars))
}

/// 编译 OpenCC 文本文件 → IUVOCC01 二进制文件（dictc opencc 子命令）。
/// 返回词条总数（phrases + chars）。
pub fn compile_files(
    phrases_path: &Path,
    chars_path: &Path,
    output: &Path,
) -> io::Result<usize> {
    let read_txt = |p: &Path| -> io::Result<String> {
        std::fs::read_to_string(p)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", p.display())))
    };
    let table = from_text(&read_txt(phrases_path)?, &read_txt(chars_path)?)?;
    let bytes = table.to_bytes();
    let mut w = io::BufWriter::new(
        std::fs::File::create(output)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", output.display())))?,
    );
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(table.entry_count())
}

fn read_u32(data: &[u8], pos: &mut usize, what: &str) -> io::Result<u32> {
    if *pos + 4 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{what}截断"),
        ));
    }
    let v = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

fn read_str(data: &[u8], pos: &mut usize, what: &str) -> io::Result<String> {
    let len = read_u32(data, pos, what)? as usize;
    if *pos + len > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{what}越界"),
        ));
    }
    let s = std::str::from_utf8(&data[*pos..*pos + len])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{what}非 UTF-8")))?
        .to_owned();
    *pos += len;
    Ok(s)
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个小测试表（短语 + 单字），用于转换逻辑断言。
    fn sample() -> OpenccTable {
        // STPhrases 风格：皇后 後；网络 → 網絡（s2t 通用繁体，非台语网络）
        let phrases = "以后\t以後\n皇后\t皇后 後\n网络\t網絡\n".
            to_string();
        let chars = "后\t后 後\n发\t发 髮\n台\t台 臺\n网\t網\n".to_string();
        from_text(&phrases, &chars).unwrap()
    }

    #[test]
    fn phrase_priority_over_char() {
        // 皇后：短语表命中 → 皇后（不落入单字 后→后/後 的首值后）
        let t = sample();
        assert_eq!(t.convert("皇后"), "皇后");
        assert_eq!(t.convert("以后"), "以後");
        assert_eq!(t.convert("网络"), "網絡");
    }

    #[test]
    fn single_char_fallback() {
        let t = sample();
        // 单字后 → 首值 后（无上下文模型，已知差距）
        assert_eq!(t.convert("后"), "后");
        // 网 → 網
        assert_eq!(t.convert("网"), "網");
        // 发 → 发（首值）
        assert_eq!(t.convert("发"), "发");
    }

    #[test]
    fn non_cjk_passthrough() {
        let t = sample();
        assert_eq!(t.convert("nihao"), "nihao");
        assert_eq!(t.convert("hello 后"), "hello 后");
        assert_eq!(t.convert("，"), "，");
        assert_eq!(t.convert(""), "");
    }

    #[test]
    fn traditional_idempotent() {
        let t = sample();
        // 已繁体字符不在简表 → 原样（幂等）
        assert_eq!(t.convert("以後網絡"), "以後網絡");
    }

    #[test]
    fn longest_match_walks_across() {
        // 短语内单字未命中也不拆分：以后 → 以後；单独后 → 后（首值）。
        let t = sample();
        assert_eq!(t.convert("以后后"), "以後后");
    }

    #[test]
    fn empty_table_passthrough() {
        let t = OpenccTable::empty();
        assert_eq!(t.convert("你好后"), "你好后");
    }

    #[test]
    fn bytes_roundtrip() {
        let t = sample();
        let bytes = t.to_bytes();
        let back = OpenccTable::from_bytes(&bytes).unwrap();
        assert_eq!(back.convert("皇后以后"), t.convert("皇后以后"));
        assert_eq!(back.convert("网"), t.convert("网"));
        assert_eq!(back.entry_count(), t.entry_count());
    }

    #[test]
    fn from_bytes_rejects_bad() {
        assert!(OpenccTable::from_bytes(b"").is_err());
        assert!(OpenccTable::from_bytes(b"NOTOPENCC").is_err());
        // magic 对但截断
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&999u32.to_le_bytes());
        assert!(OpenccTable::from_bytes(&bytes).is_err());
    }

    #[test]
    fn from_text_skips_comments_and_bom() {
        let txt = "\u{feff}# 注释\n\n以后\t以後\r\n皇后\t皇后 後\n";
        let t = from_text(txt, "").unwrap();
        assert_eq!(t.convert("以后"), "以後");
        assert_eq!(t.entry_count(), 2);
    }

    #[test]
    fn multi_value_takes_first() {
        let t = from_text("干\t干 幹\n", "").unwrap();
        assert_eq!(t.convert("干"), "干");
        // 短语干系 → 未命中短语时单字首值
        assert_eq!(t.convert("干系"), "干系");
    }
}