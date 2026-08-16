//! 用户词库：权重覆盖表 + 屏蔽表（M2，设计见 docs/plan/18-m2-user-dict.md）。
//!
//! 覆盖条目 = (code, word, adjusted_weight)——**绝对值覆盖**，无 delta 魔法数字：
//! 用户反复调整几轮后永远收敛（覆盖旧值），不可读不可预测的残留不存在。
//! 屏蔽条目 = (code, word)——基础库词条隐藏（Shift+Delete，决策：先删用户库条目，
//! 无则屏蔽基础库）。
//! 基本库（IMEDIC02 mmap 只读共享）物理不动，查询时与本表叠加（见 Dict::merged）。
//!
//! 文件为简单线性格式（小文件，不 mmap 零拷贝）：`IUVUSR01`（覆盖表）/ `IUVUSR02`
//! （覆盖表 + 屏蔽表），读侧按 magic 分派（01 兼容）。写盘 = 同目录临时文件 + sync +
//! 先删后 rename（Windows rename 不覆盖已存在文件；删除窗口内读侧 load 失败 → 保持
//! 旧库，下次会话重载，可接受——写入非高频）。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// 文件头 magic。`IUVUSR01` = 仅覆盖表（旧）；`IUVUSR02` = 覆盖表 + 屏蔽表。
const MAGIC_V1: &[u8; 8] = b"IUVUSR01";
const MAGIC_V2: &[u8; 8] = b"IUVUSR02";

/// 用户权重覆盖表 + 屏蔽表。
/// 不可变共享（Arc 写时复制：每次调整生成新实例替换，查询无锁）。
#[derive(Clone, Debug, Default)]
pub struct UserDict {
    /// 覆盖表：code → [(word, adjusted_weight)]（同 code 内 word 唯一）
    map: BTreeMap<String, Vec<(String, u32)>>,
    /// 屏蔽表：(code, word)——基础库词条隐藏（Shift+Delete）
    block: BTreeSet<(String, String)>,
}

impl UserDict {
    /// 空覆盖表（未装配/加载失败降级）。
    pub fn empty() -> UserDict {
        UserDict::default()
    }

    /// 从文件加载。缺失/损坏 → `Err`（调用方决定降级为空库，不阻断引擎）。
    pub fn load(path: &Path) -> io::Result<UserDict> {
        let data = fs::read(path)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        Self::from_bytes(&data)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    }

    /// 序列化为 `IUVUSR02` 线性字节（覆盖表段 + 屏蔽表段，magic 开头，无文件 IO）。
    /// 共享内存段（M6 shm）与 `save()` 共用同一序列化——写盘 = to_bytes + 原子替换，
    /// 共享段 = to_bytes 拷入数据区，两侧字节完全一致（`to_bytes == 文件内容` 有测试锁定）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64 + self.map.len() * 32);
        buf.extend_from_slice(MAGIC_V2);
        let count: u32 = self.map.values().map(|v| v.len()).sum::<usize>() as u32;
        buf.extend_from_slice(&count.to_le_bytes());
        for (code, entries) in &self.map {
            for (word, adj) in entries {
                buf.push(code.len() as u8);
                buf.extend_from_slice(code.as_bytes());
                buf.extend_from_slice(&(word.len() as u16).to_le_bytes());
                buf.extend_from_slice(word.as_bytes());
                buf.extend_from_slice(&adj.to_le_bytes());
            }
        }
        let block_count: u32 = self.block.len() as u32;
        buf.extend_from_slice(&block_count.to_le_bytes());
        for (code, word) in &self.block {
            buf.push(code.len() as u8);
            buf.extend_from_slice(code.as_bytes());
            buf.extend_from_slice(&(word.len() as u16).to_le_bytes());
            buf.extend_from_slice(word.as_bytes());
        }
        buf
    }

    /// 解析字节。magic 分派：
    /// - `IUVUSR01`：u32 覆盖条数 | 覆盖 × { u8 code_len|code | u16 word_len|word | u32 adj }
    /// - `IUVUSR02`：u32 覆盖条数 | 覆盖 × N | u32 屏蔽条数 | 屏蔽 × { u8 code_len|code | u16 word_len|word }
    pub fn from_bytes(data: &[u8]) -> io::Result<UserDict> {
        let bad = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());
        if data.len() < 12 {
            return Err(bad("magic 校验失败"));
        }
        let (v2, pos0) = if &data[..MAGIC_V2.len()] == MAGIC_V2 {
            (true, 8)
        } else if &data[..MAGIC_V1.len()] == MAGIC_V1 {
            (false, 8)
        } else {
            return Err(bad("magic 校验失败"));
        };
        let mut map: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
        let mut block: BTreeSet<(String, String)> = BTreeSet::new();
        let count = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
        let mut pos = pos0 + 4;
        // 通用记录读取：返回 (code, word, 尾部偏移)；cover 为 true 时多读 u32 adj。
        let read_record = |data: &[u8],
                           pos: usize,
                           cover: bool|
         -> Result<(String, String, u32, usize), String> {
            if pos >= data.len() {
                return Err("记录截断（缺 code_len）".into());
            }
            let code_len = data[pos] as usize;
            let code_end = pos + 1 + code_len;
            if code_end + 2 + 4 > data.len() {
                return Err("记录截断（code 越界）".into());
            }
            let code = std::str::from_utf8(&data[pos + 1..code_end])
                .map_err(|_| "code 非 UTF-8".to_string())?;
            let wl = u16::from_le_bytes([data[code_end], data[code_end + 1]]) as usize;
            let word_end = code_end + 2 + wl;
            // 覆盖记录尾随 u32 adj，屏蔽记录无——按 cover 检查边界
            let tail_len = if cover { 4 } else { 0 };
            if word_end + tail_len > data.len() {
                return Err("记录截断（word 越界）".into());
            }
            let word = std::str::from_utf8(&data[code_end + 2..word_end])
                .map_err(|_| "word 非 UTF-8".to_string())?;
            let mut tail = word_end;
            let adj = if cover {
                let a = u32::from_le_bytes([
                    data[word_end],
                    data[word_end + 1],
                    data[word_end + 2],
                    data[word_end + 3],
                ]);
                tail = word_end + 4;
                a
            } else {
                0
            };
            Ok((code.to_string(), word.to_string(), adj, tail))
        };
        for _ in 0..count {
            let (code, word, adj, next) = read_record(data, pos, true).map_err(|e| bad(&e))?;
            pos = next;
            map.entry(code).or_default().push((word, adj));
        }
        if v2 {
            if data.len() < pos + 4 {
                return Err(bad("屏蔽段头截断"));
            }
            let block_count =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
            pos += 4;
            for _ in 0..block_count {
                let (code, word, _, next) = read_record(data, pos, false).map_err(|e| bad(&e))?;
                pos = next;
                block.insert((code, word));
            }
        }
        Ok(UserDict { map, block })
    }

    /// code 命中的覆盖条目（未覆盖 → 空切片）。
    pub fn adjusted(&self, code: &str) -> &[(String, u32)] {
        self.map.get(code).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 一次交换：a/b 两词分别写入新权重（绝对值覆盖）。返回新 UserDict（写时复制）。
    /// 新权重由调用方计算（互写对方**合成**权重，见 Engine::swap_weights）。
    /// 双 code 签名：候选页内相邻词可能跨 code（单段档桶候选 sha/shi…同属 `sh`），
    /// 同 code 交换是 a_code == b_code 的特例。
    pub fn apply_swap(
        &self,
        a_code: &str,
        a_word: &str,
        a_adj: u32,
        b_code: &str,
        b_word: &str,
        b_adj: u32,
    ) -> UserDict {
        let mut map = self.map.clone();
        let mut upsert = |code: &str, word: &str, adj: u32| {
            let group = map.entry(code.to_string()).or_default();
            group.retain(|(w, _)| w != word);
            group.push((word.to_string(), adj));
        };
        upsert(a_code, a_word, a_adj);
        upsert(b_code, b_word, b_adj);
        UserDict {
            map,
            block: self.block.clone(),
        }
    }

    /// 写入单条目（自造词/覆盖，upsert）。返回新 UserDict（写时复制）。
    pub fn set_entry(&self, code: &str, word: &str, adj: u32) -> UserDict {
        let mut map = self.map.clone();
        let group = map.entry(code.to_string()).or_default();
        group.retain(|(w, _)| w != word);
        group.push((word.to_string(), adj));
        UserDict {
            map,
            block: self.block.clone(),
        }
    }

    /// 移除单条目（隐藏自造词/覆盖时用）。词条不存在 → 原样返回。返回新 UserDict。
    pub fn remove_entry(&self, code: &str, word: &str) -> UserDict {
        let mut map = self.map.clone();
        let mut changed = false;
        if let Some(group) = map.get_mut(code) {
            let before = group.len();
            group.retain(|(w, _)| w != word);
            changed = group.len() != before;
            if group.is_empty() {
                map.remove(code);
            }
        }
        if !changed {
            return self.clone();
        }
        UserDict {
            map,
            block: self.block.clone(),
        }
    }

    /// 屏蔽基础库词条（Shift+Delete 隐藏；幂等）。返回新 UserDict。
    pub fn block(&self, code: &str, word: &str) -> UserDict {
        let mut block = self.block.clone();
        block.insert((code.to_string(), word.to_string()));
        UserDict {
            map: self.map.clone(),
            block,
        }
    }

    /// (code, word) 是否被屏蔽。
    pub fn is_blocked(&self, code: &str, word: &str) -> bool {
        self.block.contains(&(code.to_string(), word.to_string()))
    }

    /// 覆盖表条目总数（M6 设置页显示用）。
    pub fn cover_count(&self) -> usize {
        self.map.values().map(|v| v.len()).sum()
    }

    /// 屏蔽表条目总数（M6 设置页显示用）。
    pub fn block_count(&self) -> usize {
        self.block.len()
    }

    /// 遍历全部覆盖条目 `(code, word, adj)`（M6 设置页列表用）。
    pub fn cover_iter(&self) -> impl Iterator<Item = (&str, &str, u32)> {
        self.map
            .iter()
            .flat_map(|(code, v)| v.iter().map(move |(w, a)| (code.as_str(), w.as_str(), *a)))
    }

    /// 遍历全部屏蔽条目 `(code, word)`（M6 设置页列表用）。
    pub fn block_iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.block.iter().map(|(c, w)| (c.as_str(), w.as_str()))
    }

    /// 写盘（原子，恒写 `IUVUSR02`）：同目录临时文件 + sync + 先删后 rename。
    /// 失败 → `Err`（内存态已生效，持久化可下次调整重试）。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let buf = self.to_bytes();
        let tmp = tmp_path(path);
        let mut f = fs::File::create(&tmp)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", tmp.display())))?;
        f.write_all(&buf)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", tmp.display())))?;
        // 落盘再换名：防写入后立刻断电丢失（rename 在页缓存上不保证持久）。
        f.sync_all()
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", tmp.display())))?;
        drop(f);
        let _ = fs::remove_file(path); // Windows rename 不覆盖已存在文件；失败忽略（rename 会再失败）
        fs::rename(&tmp, path)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    }
}

/// 同目录临时文件（rename 跨目录会失败）。
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("iuv-userdict-test-{}.imedic", std::process::id()));
        let _ = fs::remove_file(&path);
        let u = UserDict::empty()
            .apply_swap("haoshi", "好使", 5800, "haoshi", "耗时", 3800)
            .apply_swap("de", "的", 100, "de", "得", 100000);
        u.save(&path).unwrap();
        let back = UserDict::load(&path).unwrap();
        let adj = back.adjusted("haoshi");
        assert_eq!(adj.len(), 2);
        assert!(adj.iter().any(|(w, a)| w == "好使" && *a == 5800));
        assert!(adj.iter().any(|(w, a)| w == "耗时" && *a == 3800));
        assert!(back
            .adjusted("de")
            .iter()
            .any(|(w, a)| w == "得" && *a == 100000));
        assert!(back.adjusted("nope").is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn apply_swap_overwrites_old_value() {
        let u = UserDict::empty().apply_swap("de", "的", 100, "de", "得", 300);
        // 反复调整收敛：同词覆盖旧值，不累积
        let u = u.apply_swap("de", "的", 200, "de", "得", 500);
        let u = u.apply_swap("de", "的", 300, "de", "得", 700);
        let adj = u.adjusted("de");
        assert_eq!(adj.len(), 2, "同词反复调整只保留最新值");
        assert!(adj.iter().any(|(w, a)| w == "的" && *a == 300));
        assert!(adj.iter().any(|(w, a)| w == "得" && *a == 700));
    }

    #[test]
    fn load_missing_file_errors() {
        let path = std::env::temp_dir().join("iuv-userdict-nonexistent.imedic");
        let _ = fs::remove_file(&path);
        assert!(UserDict::load(&path).is_err());
    }

    #[test]
    fn v2_roundtrip_with_block() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("iuv-userdict-v2-{}.imedic", std::process::id()));
        let _ = fs::remove_file(&path);
        let u = UserDict::empty()
            .set_entry("zhang'wei'wei", "张葳葳", 8000)
            .block("shou'xuan", "手癣")
            .block("shou'xuan", "手癣"); // 幂等
        u.save(&path).unwrap();
        let back = UserDict::load(&path).unwrap();
        assert!(back
            .adjusted("zhang'wei'wei")
            .iter()
            .any(|(w, a)| w == "张葳葳" && *a == 8000));
        assert!(back.is_blocked("shou'xuan", "手癣"));
        assert!(!back.is_blocked("shou'xuan", "手选"));
        assert!(!back.is_blocked("zhang'wei'wei", "张葳葳"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn v1_file_compat_read() {
        // 手工构造 IUVUSR01 字节（覆盖表，无屏蔽段）："de"→得 100000
        let mut buf = b"IUVUSR01".to_vec();
        buf.extend_from_slice(&[1, 0, 0, 0]);
        buf.push(2);
        buf.extend_from_slice(b"de");
        buf.extend_from_slice(&3u16.to_le_bytes()); // "得" UTF-8 3 字节
        buf.extend_from_slice("得".as_bytes());
        buf.extend_from_slice(&100000u32.to_le_bytes());
        let dir = std::env::temp_dir();
        let path = dir.join(format!("iuv-userdict-v1-{}.imedic", std::process::id()));
        fs::write(&path, &buf).unwrap();
        let u = UserDict::load(&path).unwrap();
        assert!(u
            .adjusted("de")
            .iter()
            .any(|(w, a)| w == "得" && *a == 100000));
        assert!(!u.is_blocked("de", "得"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn remove_entry_removes_self_word() {
        let u = UserDict::empty()
            .set_entry("de", "的", 100)
            .set_entry("de", "得", 200)
            .set_entry("nihao", "你好", 500);
        // 移除组内一条：另一条保留
        let u = u.remove_entry("de", "得");
        assert!(!u.adjusted("de").iter().any(|(w, _)| w == "得"));
        assert!(u.adjusted("de").iter().any(|(w, _)| w == "的"));
        // 移除最后一条：组消失
        let u = u.remove_entry("de", "的");
        assert!(u.adjusted("de").is_empty());
        // 移除不存在的词条：原样返回（含其他组不受影响）
        let u = u.remove_entry("nihao", "不存在");
        assert!(u.adjusted("nihao").iter().any(|(w, _)| w == "你好"));
        // 屏蔽表在写时复制中保留
        let u = u.block("de", "的");
        let u2 = u.remove_entry("nihao", "你好");
        assert!(u2.is_blocked("de", "的"));
    }

    #[test]
    fn load_bad_data_errors() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("iuv-userdict-bad-{}.imedic", std::process::id()));
        fs::write(&path, b"not-a-dict").unwrap();
        assert!(UserDict::load(&path).is_err());
        // 截断：magic 对但记录半截
        let mut bad = b"IUVUSR01".to_vec();
        bad.extend_from_slice(&[1, 0, 0, 0]);
        bad.extend_from_slice(&[3, b'n', b'i']);
        fs::write(&path, bad).unwrap();
        assert!(UserDict::load(&path).is_err());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn to_bytes_from_bytes_roundtrip() {
        let u = UserDict::empty()
            .apply_swap("haoshi", "好使", 5800, "haoshi", "耗时", 3800)
            .set_entry("zhang'wei'wei", "张葳葳", 8000)
            .block("shou'xuan", "手癣");
        let bytes = u.to_bytes();
        assert!(bytes.starts_with(MAGIC_V2), "恒写 IUVUSR02");
        let back = UserDict::from_bytes(&bytes).unwrap();
        assert_eq!(back.to_bytes(), bytes, "再序列化字节完全一致（无歧义编码）");
        assert!(back
            .adjusted("haoshi")
            .iter()
            .any(|(w, a)| w == "好使" && *a == 5800));
        assert!(back
            .adjusted("zhang'wei'wei")
            .iter()
            .any(|(w, a)| w == "张葳葳" && *a == 8000));
        assert!(back.is_blocked("shou'xuan", "手癣"));
    }

    #[test]
    fn to_bytes_matches_save_file_content() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("iuv-userdict-bytes-{}.imedic", std::process::id()));
        let _ = fs::remove_file(&path);
        let u = UserDict::empty()
            .set_entry("nihao", "你好", 9000)
            .block("de", "的");
        u.save(&path).unwrap();
        let file_bytes = fs::read(&path).unwrap();
        assert_eq!(file_bytes, u.to_bytes(), "文件内容 == to_bytes（共享段/写盘同字节）");
        let _ = fs::remove_file(&path);
    }
}
