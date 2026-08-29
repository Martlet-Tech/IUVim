//! IMEDIC02 平面词库格式：段表驱动，加载零加工（mmap 直读）。见 docs/plan/17-imedic02-mmap.md。
//!
//! 布局：
//! ```text
//! [0..8]   magic = b"IMEDIC02"
//! [8..12]  u32 LE 段数 N
//! [12..]   段表：N × { u8 段类型 | u32 偏移 | u32 长度 }（偏移相对文件头）
//! 段1 元数据:  u64 total_weight | u32 entry_count | u32 max_word_syllables
//!             | u32 音节数 | 音节 × { u8 len | bytes（UTF-8） }
//! 段2 首字母桶: 26 × { u8 字母 | u32 记录数 | 记录 × N }（单字，weight 降序，≤INITIAL_BUCKET_SIZE/桶）
//! 段3 记录索引: record_count × u32 记录体段内偏移（按 code 升序）
//! 段4 记录体:   record_count × { u8 code_len | code | u16 word_len | word | u32 weight }
//! ```
//! 记录排序不变量（code 升序、组内 weight 降序）由写端保证；加载只做简单边界检查
//! （不校验排序/单调性——数据出自自家 dictc，防的是截断与坏字节）。
//! 段表驱动：未来追加段（屏蔽段/用户段）= 新段类型，旧加载器忽略未知段，双向兼容。

use crate::mmap::MappedFile;
use crate::{Dict, Entry, INITIAL_BUCKET_SIZE};
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// 文件头 magic。
pub const MAGIC: &[u8; 8] = b"IMEDIC02";

// 段类型（1..=4 为当前必需段；未知段加载时忽略）
pub(crate) const SEG_META: u8 = 1;
pub(crate) const SEG_BUCKETS: u8 = 2;
pub(crate) const SEG_INDEX: u8 = 3;
pub(crate) const SEG_RECORDS: u8 = 4;
pub(crate) const SEG_HEADER_LEN: usize = 1 + 4 + 4; // u8 类型 | u32 偏移 | u32 长度
pub(crate) const FILE_HEADER_LEN: usize = 8 + 4; // magic | u32 段数

/// 从二进制文件加载词典：mmap 零拷贝 + 段表定位 + 全量边界校验。
/// magic 校验失败 / 截断 / 坏数据 → `io::ErrorKind::InvalidData`。
pub fn load(path: &Path) -> io::Result<Dict> {
    let file = MappedFile::open(path)?;
    Dict::from_file(file).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// 标准汉语拼音音节表（无调，按字母序，供二分查找）。
/// 数据来源：现代汉语拼音方案常用音节；不含语气词音节（m/n/ng/hm/hng 等）。
pub(crate) static SYLLABLES: &[&str] = &[
    "a", "ai", "an", "ang", "ao", "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi",
    "bian", "biao", "bie", "bin", "bing", "bo", "bu", "ca", "cai", "can", "cang", "cao", "ce",
    "cei", "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi",
    "chong", "chou", "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci",
    "cong", "cou", "cu", "cuan", "cui", "cun", "cuo", "da", "dai", "dan", "dang", "dao", "de",
    "dei", "den", "deng", "di", "dia", "dian", "diao", "die", "ding", "diu", "dong", "dou", "du",
    "duan", "dui", "dun", "duo", "e", "ei", "en", "eng", "er", "fa", "fan", "fang", "fei", "fen",
    "feng", "fo", "fou", "fu", "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng",
    "gong", "gou", "gu", "gua", "guai", "guan", "guang", "gui", "gun", "guo", "ha", "hai", "han",
    "hang", "hao", "he", "hei", "hen", "heng", "hong", "hou", "hu", "hua", "huai", "huan", "huang",
    "hui", "hun", "huo", "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong",
    "jiu", "ju", "juan", "jue", "jun", "ka", "kai", "kan", "kang", "kao", "ke", "ken", "keng",
    "kong", "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo", "la", "lai", "lan",
    "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin", "ling",
    "liu", "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve", "ma", "mai", "man",
    "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu",
    "mo", "mou", "mu", "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni", "nian",
    "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou", "nu", "nuan", "nuo", "nv", "nve",
    "o", "ou", "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao",
    "pie", "pin", "ping", "po", "pou", "pu", "qi", "qia", "qian", "qiang", "qiao", "qie", "qin",
    "qing", "qiong", "qiu", "qu", "quan", "que", "qun", "ran", "rang", "rao", "re", "ren", "reng",
    "ri", "rong", "rou", "ru", "ruan", "rui", "run", "ruo", "sa", "sai", "san", "sang", "sao",
    "se", "sen", "seng", "sha", "shai", "shan", "shang", "shao", "she", "shei", "shen", "sheng",
    "shi", "shou", "shu", "shua", "shuai", "shuan", "shuang", "shui", "shun", "shuo", "si", "song",
    "sou", "su", "suan", "sui", "sun", "suo", "ta", "tai", "tan", "tang", "tao", "te", "teng",
    "ti", "tian", "tiao", "tie", "ting", "tong", "tou", "tu", "tuan", "tui", "tun", "tuo", "wa",
    "wai", "wan", "wang", "wei", "wen", "weng", "wo", "wu", "xi", "xia", "xian", "xiang", "xiao",
    "xie", "xin", "xing", "xiong", "xiu", "xu", "xuan", "xue", "xun", "ya", "yan", "yang", "yao",
    "ye", "yi", "yin", "ying", "yo", "yong", "you", "yu", "yuan", "yue", "yun", "za", "zai", "zan",
    "zang", "zao", "ze", "zei", "zen", "zeng", "zha", "zhai", "zhan", "zhang", "zhao", "zhe",
    "zhei", "zhen", "zheng", "zhi", "zhong", "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang",
    "zhui", "zhun", "zhuo", "zi", "zong", "zou", "zu", "zuan", "zui", "zun", "zuo",
];

/// 判定一段字符串是否为标准合法音节（二分查找）。
pub(crate) fn is_syllable(s: &str) -> bool {
    SYLLABLES.binary_search(&s).is_ok()
}

/// 贪心最长匹配切分（与 Quanpin 同一规则）：返回 (音节序列, 音节数)。
/// `'` 为强制分隔（不产生段）；匹配失败的单字母原样保留，保证对任意输入不 panic。
/// üe 去点输入形归一（lue→lve、nue→nve，同 Quanpin 的 24-ue-input-alias.md 规则）。
pub(crate) fn greedy_segment(code: &str) -> Vec<String> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\'' {
            i += 1; // 强制分隔，不产生段
            continue;
        }
        let rem = b.len() - i;
        let mut matched = false;
        // 最长音节不超过 6 个字符（zhuang/chuang）
        for len in (1..=rem.min(6)).rev() {
            if is_syllable(&code[i..i + len]) {
                let syl = &code[i..i + len];
                let syl = match syl {
                    "lue" => "lve",
                    "nue" => "nve",
                    _ => syl,
                };
                out.push(syl.to_string());
                i += len;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push(code[i..i + 1].to_string());
            i += 1;
        }
    }
    out
}

/// 写入 IMEDIC02。records 不要求有序——本函数内部排序并保证排序不变量
/// （code 升序；同 code 按 weight 降序、同 weight 按 word 升序）。
pub fn write(records: &[Entry], writer: impl io::Write) -> io::Result<()> {
    let mut records: Vec<Entry> = records.to_vec();
    records.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then_with(|| b.weight.cmp(&a.weight))
            .then_with(|| a.word.cmp(&b.word))
    });

    // ---- 元数据（total/entry_count/max_word_syllables/音节表；逻辑与 from_entries 一致）----
    let mut total = 0u64;
    let mut max_word_syllables = 0usize;
    let mut syllables: Vec<String> = Vec::new();
    let mut syllable_set = std::collections::BTreeSet::new();
    for r in &records {
        total += r.weight as u64;
        let seg = greedy_segment(&r.code);
        // 仅全为合法音节的词条计入词长（英文条目如 "abc" 不算拼音词）
        if seg.iter().all(|s| is_syllable(s)) {
            max_word_syllables = max_word_syllables.max(seg.len());
        }
        for s in &seg {
            if is_syllable(s) {
                syllable_set.insert(s.clone());
            }
        }
    }
    // üe 韵母的去点输入形（l/n 侧别名；j/q/x/y 侧 jue/que/xue/yue 已在表中）。
    // 非标准音节，只作输入识别；运行时 Quanpin 靠它切出 lüe/nüe 路径并归一为 v 形。
    syllable_set.insert("lue".to_string());
    syllable_set.insert("nue".to_string());
    syllables.extend(syllable_set);

    // ---- 首字母桶（单遍收集副本 + 桶内排序截断；只收单字：word 单字且 code 无 `'`）----
    let mut buckets: Vec<Vec<Entry>> = (0..26).map(|_| Vec::new()).collect();
    for e in &records {
        if e.word.chars().count() == 1 && !e.code.contains('\'') {
            if let Some(c) = e.code.chars().next() {
                if c.is_ascii_lowercase() {
                    buckets[(c as u8 - b'a') as usize].push(e.clone());
                }
            }
        }
    }
    for v in buckets.iter_mut() {
        v.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
        v.truncate(INITIAL_BUCKET_SIZE);
    }

    // ---- 序列化段（一次性字节组装）----
    let meta = {
        let mut m = Vec::new();
        m.extend_from_slice(&total.to_le_bytes());
        m.extend_from_slice(&(records.len() as u32).to_le_bytes());
        m.extend_from_slice(&(max_word_syllables as u32).to_le_bytes());
        m.extend_from_slice(&(syllables.len() as u32).to_le_bytes());
        for s in &syllables {
            let len = u8::try_from(s.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "音节超过 255 字节"))?;
            m.push(len);
            m.extend_from_slice(s.as_bytes());
        }
        m
    };

    let bucket_seg = {
        let mut b = Vec::new();
        for (i, v) in buckets.iter().enumerate() {
            b.push(b'a' + i as u8);
            b.extend_from_slice(&(v.len() as u32).to_le_bytes());
            for e in v {
                write_record(&mut b, e)?;
            }
        }
        b
    };

    let (index_seg, records_seg) = {
        let mut idx = Vec::with_capacity(records.len() * 4);
        let mut body = Vec::new();
        for e in &records {
            let off = u32::try_from(body.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "记录体超过 4GB"))?;
            idx.extend_from_slice(&off.to_le_bytes());
            write_record(&mut body, e)?;
        }
        (idx, body)
    };

    // ---- 段表 + 文件头（偏移在组装后计算）----
    let segs: [(&[u8], u8); 4] = [
        (&meta, SEG_META),
        (&bucket_seg, SEG_BUCKETS),
        (&index_seg, SEG_INDEX),
        (&records_seg, SEG_RECORDS),
    ];
    let mut offset = FILE_HEADER_LEN + segs.len() * SEG_HEADER_LEN;
    let mut header = Vec::with_capacity(FILE_HEADER_LEN + segs.len() * SEG_HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&(segs.len() as u32).to_le_bytes());
    for (seg, ty) in &segs {
        header.push(*ty);
        header.extend_from_slice(&(offset as u32).to_le_bytes());
        header.extend_from_slice(&(seg.len() as u32).to_le_bytes());
        offset += seg.len();
    }

    let mut w = BufWriter::new(writer);
    w.write_all(&header)?;
    for (seg, _) in &segs {
        w.write_all(seg)?;
    }
    w.flush()
}

/// 单条记录序列化（code/word 长度上限分别 255/65535 字节）。
fn write_record(buf: &mut Vec<u8>, e: &Entry) -> io::Result<()> {
    let code = e.code.as_bytes();
    let code_len = u8::try_from(code.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "code 超过 255 字节"))?;
    buf.push(code_len);
    buf.extend_from_slice(code);
    let word = e.word.as_bytes();
    let word_len = u16::try_from(word.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "word 超过 65535 字节"))?;
    buf.extend_from_slice(&word_len.to_le_bytes());
    buf.extend_from_slice(word);
    buf.extend_from_slice(&e.weight.to_le_bytes());
    Ok(())
}
