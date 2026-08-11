//! rime `.dict.yaml` → 记录集 编译。契约 01-contract.md §3、任务书 10 §3.1。

use crate::{format, Entry};
use std::collections::{btree_map::Entry as MapEntry, BTreeMap};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileStats {
    pub files: usize,
    pub entries: usize,
    pub codes: usize,
    pub duplicates: usize,
}

/// 解析 rime .dict.yaml 文件列表，合并去重，写二进制到 output。
pub fn compile_files(inputs: &[PathBuf], output: &Path) -> io::Result<CompileStats> {
    if inputs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "compile_files: 输入文件列表为空",
        ));
    }
    let mut uniq: BTreeMap<(String, String), u32> = BTreeMap::new();
    let mut duplicates = 0usize;
    for input in inputs {
        duplicates += parse_file(input, &mut uniq)?;
    }
    let mut records: Vec<Entry> = uniq
        .into_iter()
        .map(|((code, word), weight)| Entry { word, code, weight })
        .collect();
    // 契约 §3.1：按 (code 升序, weight 降序) 排列写入；weight 相同再按 word 保证确定序。
    records.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then_with(|| b.weight.cmp(&a.weight))
            .then_with(|| a.word.cmp(&b.word))
    });
    let mut codes = 0usize;
    let mut last_code: Option<&str> = None;
    for r in &records {
        if last_code != Some(r.code.as_str()) {
            codes += 1;
            last_code = Some(r.code.as_str());
        }
    }
    let file = File::create(output)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", output.display())))?;
    format::write(&records, file)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", output.display())))?;
    Ok(CompileStats {
        files: inputs.len(),
        entries: records.len(),
        codes,
        duplicates,
    })
}

/// 解析单个 rime .dict.yaml 文件，将 (squashed_code, word) → 最大 weight 归入 `uniq`，
/// 返回该文件贡献的重复条目数。
///
/// 语法（任务书 10 §3.1）：
/// - `#` 开头为注释；`---` 与单独一行 `...` 之间为 yaml 头部（跳过）；容忍 BOM 与 CRLF
/// - 词条行：`词<TAB>带空格拼音<TAB>权重`，权重可缺省（按 0）
/// - 空行与字段数 < 2 的行忽略；拼音去空白转小写作为查询键（squashed）
fn parse_file(path: &Path, uniq: &mut BTreeMap<(String, String), u32>) -> io::Result<usize> {
    let file = File::open(path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    let mut in_header = false;
    let mut duplicates = 0usize;
    for (idx, line) in BufReader::new(file).lines().enumerate() {
        let lineno = idx + 1;
        let line = line.map_err(|e| {
            io::Error::new(e.kind(), format!("{}: 第{lineno}行: {e}", path.display()))
        })?;
        let line = line.strip_prefix('\u{feff}').unwrap_or(&line);
        let line = line.trim_end_matches('\r').trim();
        if line == "---" {
            in_header = true;
            continue;
        }
        if line == "..." {
            in_header = false;
            continue;
        }
        if in_header || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            continue;
        }
        let word = fields[0];
        if word.is_empty() {
            continue;
        }
        let code = squash(fields[1]);
        if code.is_empty() {
            continue;
        }
        let weight = match fields.get(2) {
            Some(s) if !s.trim().is_empty() => s.trim().parse::<u32>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: 第{lineno}行: 权重不是非负整数: {s:?}", path.display()),
                )
            })?,
            _ => 0,
        };
        match uniq.entry((code, word.to_string())) {
            MapEntry::Occupied(mut e) => {
                duplicates += 1;
                *e.get_mut() = (*e.get_mut()).max(weight);
            }
            MapEntry::Vacant(e) => {
                e.insert(weight);
            }
        }
    }
    Ok(duplicates)
}

/// 拼音列 → 词条键：空白转 `'`、转小写（`ni hao` → `ni'hao`；`xi an` → `xi'an`）。
/// 保留音节分隔信息：无撇号输入经枚举切分重建 `'` 键命中词条，
/// 强制输入 `xi'an` 直接命中 `xi'an` 键（单字词无空白，键不变）。
fn squash(pinyin: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for c in pinyin.chars() {
        if c.is_whitespace() {
            pending_sep = true;
        } else {
            if pending_sep && !out.is_empty() {
                out.push('\'');
            }
            pending_sep = false;
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}
