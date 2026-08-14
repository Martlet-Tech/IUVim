//! 用户词库：权重覆盖表（M2 主动调权，设计见 docs/plan/18-m2-user-dict.md）。
//!
//! 覆盖条目 = (code, word, adjusted_weight)——**绝对值覆盖**，无 delta 魔法数字：
//! 用户反复调整几轮后永远收敛（覆盖旧值），不可读不可预测的残留不存在。
//! 基本库（IMEDIC02 mmap 只读共享）物理不动，查询时与本表叠加（见 Dict::merged）。
//!
//! 文件为简单线性格式（小文件，不 mmap 零拷贝）：`IUVUSR01` magic + 条数 + 记录。
//! 写盘 = 同目录临时文件 + sync + 先删后 rename（Windows rename 不覆盖已存在文件；
//! 删除窗口内读侧 load 失败 → 保持旧库，下次会话重载，可接受——写入非高频）。

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// 文件头 magic（格式演进升 `IUVUSR02`，读侧按 magic 分派）。
const MAGIC: &[u8; 8] = b"IUVUSR01";

/// 用户权重覆盖表：code → [(word, adjusted_weight)]（同 code 内 word 唯一）。
/// 不可变共享（Arc 写时复制：每次调整生成新实例替换，查询无锁）。
#[derive(Clone, Debug, Default)]
pub struct UserDict {
    map: BTreeMap<String, Vec<(String, u32)>>,
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

    /// 解析字节。布局：magic(8) | u32 LE 条数 | 条数 × { u8 code_len|code |
    /// u16 LE word_len|word | u32 LE adjusted }。
    fn from_bytes(data: &[u8]) -> io::Result<UserDict> {
        let bad = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());
        if data.len() < 12 || &data[..MAGIC.len()] != MAGIC {
            return Err(bad("magic 校验失败"));
        }
        let count = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
        let mut map: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
        let mut pos = 12;
        for _ in 0..count {
            if pos >= data.len() {
                return Err(bad("记录截断（缺 code_len）"));
            }
            let code_len = data[pos] as usize;
            pos += 1;
            let code_end = pos + code_len;
            if code_end + 2 + 4 > data.len() {
                return Err(bad("记录截断（code 越界）"));
            }
            let code =
                std::str::from_utf8(&data[pos..code_end]).map_err(|_| bad("code 非 UTF-8"))?;
            pos = code_end;
            let word_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;
            let word_end = pos + word_len;
            if word_end + 4 > data.len() {
                return Err(bad("记录截断（word 越界）"));
            }
            let word =
                std::str::from_utf8(&data[pos..word_end]).map_err(|_| bad("word 非 UTF-8"))?;
            pos = word_end;
            let adj = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            pos += 4;
            map.entry(code.to_string())
                .or_default()
                .push((word.to_string(), adj));
        }
        Ok(UserDict { map })
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
        UserDict { map }
    }

    /// 写盘（原子）：同目录临时文件 + sync + 先删后 rename。
    /// 失败 → `Err`（内存态已生效，持久化可下次调整重试）。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut buf = Vec::with_capacity(64 + self.map.len() * 32);
        buf.extend_from_slice(MAGIC);
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
}
