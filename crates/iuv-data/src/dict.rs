//! Dict 查询层：IMEDIC02 平面词库的 mmap 零加工查询。
//! 加载 = mmap + 段表定位 + 一次边界校验扫描；查询 = 索引段二分 + 记录体物化。
//! 接口契约 01-contract.md §3（`exact` 系列返回物化 `Vec<Entry>`）。

use crate::format::{
    self, FILE_HEADER_LEN, MAGIC, SEG_BUCKETS, SEG_HEADER_LEN, SEG_INDEX, SEG_META, SEG_RECORDS,
};
use crate::mmap::MappedFile;
use crate::userdict::UserDict;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::ops::Range;
use std::sync::{Arc, Mutex};

/// 词条。`code` 为 squashed 全拼（无空格全小写，音节间 `'` 分隔），与查询键同形。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub word: String,
    pub code: String,
    pub weight: u32,
}

/// 每首字母桶上限（单段档候选池；M1.5 修正：桶只收**单字**——多字词在单段档
/// 永远用不上，占桶位只会挤掉同音字，故不收录。常量可调，太大徒增词库文件）。
pub const INITIAL_BUCKET_SIZE: usize = 1000;

/// 内存词典：mmap 视图 + 段偏移。查询全在 `file` 视图上做（物化 Entry 拷贝）。
/// Clone 共享同一映射（Arc），语义同原 BTreeMap 版本。
#[derive(Debug)]
pub struct Dict {
    file: Arc<MappedFile>,
    /// 段2 首字母桶目录：26 项 (字母, 桶段内起始偏移, 记录数)，按 a-z。
    bucket_dir: Vec<(u8, u32, u32)>,
    /// 段3 记录索引（记录体段内偏移数组）
    index: Range<usize>,
    /// 段4 记录体
    records: Range<usize>,
    /// 段2 首字母桶（桶记录内联于此段）
    buckets: Range<usize>,
    total: u64,
    entry_count: usize,
    max_word_syllables: usize,
    syllables: BTreeSet<String>,
    /// 用户权重覆盖表（M2 主动调权，18-m2-user-dict.md）。None = 未装配；
    /// Arc 写时复制（swap 整体替换），查询只 clone 引用（无锁读）。
    user: Mutex<Option<Arc<UserDict>>>,
}

impl Clone for Dict {
    fn clone(&self) -> Dict {
        Dict {
            file: self.file.clone(),
            bucket_dir: self.bucket_dir.clone(),
            index: self.index.clone(),
            records: self.records.clone(),
            buckets: self.buckets.clone(),
            total: self.total,
            entry_count: self.entry_count,
            max_word_syllables: self.max_word_syllables,
            syllables: self.syllables.clone(),
            user: Mutex::new(self.user.lock().unwrap_or_else(|e| e.into_inner()).clone()),
        }
    }
}

impl Default for Dict {
    fn default() -> Self {
        // 空文件无法解析（magic 缺失），直接构造空状态；所有查询自然返回空。
        Dict {
            file: Arc::new(MappedFile::from_vec(Vec::new())),
            bucket_dir: Vec::new(),
            index: 0..0,
            records: 0..0,
            buckets: 0..0,
            total: 0,
            entry_count: 0,
            max_word_syllables: 0,
            syllables: BTreeSet::new(),
            user: Mutex::new(None),
        }
    }
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
                out.push(code[i..i + len].to_string());
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

// ===== 字节级读取（视图已由 from_file 全量校验，索引操作安全）=====

fn u32_at(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

impl Dict {
    /// 从 mmap/内存字节解析（统一加载路径；含全量边界校验扫描）。
    /// 校验内容：头部/magic、段表边界、各段内部逐条边界（无分配）；不校验排序不变量。
    pub(crate) fn from_file(file: MappedFile) -> io::Result<Dict> {
        let bytes = file.as_bytes();
        let bad = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);

        if bytes.len() < FILE_HEADER_LEN {
            return Err(bad(format!("文件过短（{} 字节）", bytes.len())));
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(bad(format!("magic 校验失败（期望 {MAGIC:?}）")));
        }
        let seg_count = u32_at(bytes, 8) as usize;
        let table_end = FILE_HEADER_LEN + seg_count * SEG_HEADER_LEN;
        if table_end > bytes.len() {
            return Err(bad("段表越界（文件截断）".into()));
        }
        // 段表 → (类型, 段区间)
        let mut segs: Vec<(u8, Range<usize>)> = Vec::with_capacity(seg_count);
        for i in 0..seg_count {
            let h = FILE_HEADER_LEN + i * SEG_HEADER_LEN;
            let ty = bytes[h];
            let off = u32_at(bytes, h + 1) as usize;
            let len = u32_at(bytes, h + 5) as usize;
            let end = off.checked_add(len).filter(|e| *e <= bytes.len());
            let end = end.ok_or_else(|| bad("段偏移越界".into()))?;
            segs.push((ty, off..end));
        }
        let meta = seg_of(&segs, SEG_META).ok_or_else(|| bad("缺少元数据段".into()))?;
        let index = seg_of(&segs, SEG_INDEX).ok_or_else(|| bad("缺少记录索引段".into()))?;
        let records = seg_of(&segs, SEG_RECORDS).ok_or_else(|| bad("缺少记录体段".into()))?;
        let buckets = seg_of(&segs, SEG_BUCKETS).ok_or_else(|| bad("缺少首字母桶段".into()))?;
        if index.len() % 4 != 0 {
            return Err(bad("索引段长度不是 4 的倍数".into()));
        }

        // ---- 段1 元数据（u64 total | u32 entry | u32 max_syl | u32 音节数 | 音节×{u8 len, bytes}）----
        let m = meta.clone();
        if m.len() < 20 {
            return Err(bad("元数据段过短".into()));
        }
        let total = u64::from_le_bytes([
            bytes[m.start],
            bytes[m.start + 1],
            bytes[m.start + 2],
            bytes[m.start + 3],
            bytes[m.start + 4],
            bytes[m.start + 5],
            bytes[m.start + 6],
            bytes[m.start + 7],
        ]);
        let entry_count = u32_at(bytes, m.start + 8) as usize;
        let max_word_syllables = u32_at(bytes, m.start + 12) as usize;
        let syl_count = u32_at(bytes, m.start + 16) as usize;
        let mut syllables = BTreeSet::new();
        let mut pos = m.start + 20;
        for _ in 0..syl_count {
            if pos >= m.end {
                return Err(bad("音节表截断".into()));
            }
            let len = bytes[pos] as usize;
            pos += 1;
            let end = pos + len;
            if end > m.end {
                return Err(bad("音节表截断".into()));
            }
            let s =
                std::str::from_utf8(&bytes[pos..end]).map_err(|_| bad("音节非 UTF-8".into()))?;
            syllables.insert(s.to_string());
            pos = end;
        }

        // ---- 段3 索引：每条偏移 < 记录体长度 ----
        let record_count = index.len() / 4;
        let records_len = records.end - records.start;
        for i in 0..record_count {
            let off = u32_at(bytes, index.start + i * 4) as usize;
            if off >= records_len {
                return Err(bad("索引偏移越界".into()));
            }
        }

        // ---- 段4 记录体：逐条边界扫描（防截断/坏字节；不校验排序）----
        let mut pos = records.start;
        while pos < records.end {
            let step = record_step(&bytes[pos..records.end]).map_err(|m| bad(m))?;
            pos += step;
        }

        // ---- 段2 首字母桶：26 桶头部 + 逐条边界扫描；目录物化 ----
        let mut bucket_dir = Vec::with_capacity(26);
        let mut pos = buckets.start;
        let mut count = 0usize;
        while pos < buckets.end {
            if buckets.end - pos < 5 {
                return Err(bad("桶段头部截断".into()));
            }
            let letter = bytes[pos];
            if !letter.is_ascii_lowercase() {
                return Err(bad("桶字母非法".into()));
            }
            let n = u32_at(bytes, pos + 1) as usize;
            // 记录数下限检查：每条记录至少 7 字节
            if n > (buckets.end - pos - 5) / 7 {
                return Err(bad("桶记录数越界".into()));
            }
            bucket_dir.push((letter, (pos + 5 - buckets.start) as u32, n as u32));
            pos += 5;
            for _ in 0..n {
                let step = record_step(&bytes[pos..buckets.end]).map_err(|m| bad(m))?;
                pos += step;
            }
            count += 1;
        }
        if count != 26 {
            return Err(bad(format!("桶段应含 26 个桶，实际 {count}")));
        }

        Ok(Dict {
            file: Arc::new(file),
            bucket_dir,
            index: index.clone(),
            records: records.clone(),
            buckets: buckets.clone(),
            total,
            entry_count,
            max_word_syllables,
            syllables,
            user: Mutex::new(None),
        })
    }

    /// 测试/用户词库构造器。items = (squashed_code, word, weight)。
    /// 同码多条按 weight 降序归并；同 (code,word) 去重取最大 weight。
    /// 实现 = 归并 → 序列化 IMEDIC02 → 统一解析路径（与文件加载完全同构）。
    pub fn from_entries(items: Vec<(String, String, u32)>) -> Dict {
        let mut map: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        for (code, word, weight) in items {
            let group = map.entry(code.clone()).or_default();
            if let Some(prev) = group.iter_mut().find(|e| e.word == word) {
                prev.weight = prev.weight.max(weight);
            } else {
                group.push(Entry { word, code, weight });
            }
        }
        let records: Vec<Entry> = map.into_values().flatten().collect();
        let mut buf = Vec::new();
        format::write(&records, &mut buf).expect("内存序列化不可能失败");
        Dict::from_file(MappedFile::from_vec(buf)).expect("自产数据必合法")
    }

    /// 精确查询：squashed_code 如 "nihao"。返回按 weight 降序（写端不变量）的物化词条。
    pub fn exact(&self, squashed_code: &str) -> Vec<Entry> {
        if !squashed_code.is_empty() {
            return self.merged(squashed_code, self.exact_raw(squashed_code));
        }
        self.exact_raw(squashed_code)
    }

    /// 基础库精确查询（不过屏蔽/覆盖/独有条目——供 effective_weight 等需要
    /// "词条真实存在性"的内部语义使用；外部一律走 exact 的叠加视图）。
    fn exact_raw(&self, squashed_code: &str) -> Vec<Entry> {
        let target = squashed_code.as_bytes();
        let n = self.index.len() / 4;
        if n == 0 {
            return Vec::new();
        }
        let lower = self.lower_bound(target);
        if lower == n || self.code_at(self.index_off(lower)) != target {
            return Vec::new();
        }
        let upper = self.upper_bound(target);
        (lower..upper)
            .map(|i| self.entry_at(self.index_off(i)))
            .collect()
    }

    /// 精确查询（单字视图，M1.5 单段档）：返回 code == squashed_code 的**单字**词条。
    pub fn exact_single(&self, squashed_code: &str) -> Vec<Entry> {
        self.exact(squashed_code)
            .into_iter()
            .filter(|e| e.word.chars().count() == 1)
            .collect()
    }

    /// 前缀补全：返回 squashed 以 prefix 开头（且不等于 prefix）的词条，
    /// 跨编码按 weight 降序，最多 limit 条。低频路径（默认关闭），实现为
    /// 范围物化 + 全量排序；如开启后性能不达标再改归并取 top-k。
    pub fn prefix(&self, squashed_prefix: &str, limit: usize) -> Vec<Entry> {
        if limit == 0 || squashed_prefix.is_empty() {
            return Vec::new();
        }
        let target = squashed_prefix.as_bytes();
        let n = self.index.len() / 4;
        let mut out = Vec::new();
        for i in self.lower_bound(target)..n {
            let code = self.code_at(self.index_off(i));
            if !code.starts_with(target) {
                break;
            }
            if code == target {
                continue;
            }
            out.push(self.entry_at(self.index_off(i)));
        }
        out = self.merged("", out);
        out.truncate(limit);
        out
    }

    /// 首字母桶查询：返回 code 以 `initial` 开头的**单字**词条，按词频降序，
    /// 最多 `limit` 条。桶在编译期预建（每字母 top-1000 单字），M1.5 单段输入档
    /// （`c`/`sh`/`shi`…）用它取代全表前缀扫描（'s' 全扫 10 万条再排序不可用）。
    pub fn initial_top(&self, initial: char, limit: usize) -> Vec<Entry> {
        if limit == 0 || !initial.is_ascii_lowercase() {
            return Vec::new();
        }
        let idx = (initial as u8 - b'a') as usize;
        let Some(&(_, off, count)) = self.bucket_dir.get(idx) else {
            return Vec::new();
        };
        let take = (count as usize).min(limit);
        let mut out = Vec::with_capacity(take);
        let mut pos = self.buckets.start + off as usize;
        for _ in 0..take {
            let (e, step) = self.entry_at_with_step(pos);
            out.push(e);
            pos = step;
        }
        out = self.merged("", out);
        out.truncate(limit);
        out
    }

    /// 全部音节集合（编译期固化在元数据段，加载物化），供全拼切分器构造。
    pub fn syllables(&self) -> &BTreeSet<String> {
        &self.syllables
    }

    /// 全部词条 weight 之和（LM 分母）。
    pub fn total_weight(&self) -> u64 {
        self.total
    }

    /// 词条总数。
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// 最长词的音节数（lattice 宽度上限）。
    pub fn max_word_syllables(&self) -> usize {
        self.max_word_syllables
    }

    // ===== 用户权重覆盖表（M2 主动调权，18-m2-user-dict.md）=====

    /// 装配用户覆盖表（Arc 引用；调整时整体替换，查询零锁）。
    pub fn set_user(&self, user: Arc<UserDict>) {
        *self.user.lock().unwrap_or_else(|e| e.into_inner()) = Some(user);
    }

    /// 当前用户覆盖表（未装配 → None）。
    pub fn user(&self) -> Option<Arc<UserDict>> {
        self.user.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 词条有效权重：用户覆盖值优先（含自造词——覆盖表对词条来源不区分），
    /// 否则基本库权重。均无 → None。屏蔽不影响（展示层语义，词条本身仍在）。
    pub fn effective_weight(&self, code: &str, word: &str) -> Option<u32> {
        let user = self.user();
        if let Some(adj) = user.as_ref().and_then(|u| {
            u.adjusted(code)
                .iter()
                .find(|(w, _)| w == word)
                .map(|(_, a)| *a)
        }) {
            return Some(adj);
        }
        self.exact_raw(code)
            .into_iter()
            .find(|e| e.word == word)
            .map(|e| e.weight)
    }

    /// 应用用户覆盖 + 稳定排序（同 weight 保持输入原序）。未装配 → 快速路径原样返回。
    /// 叠加三合一（M2）：① 屏蔽过滤（基础库词条隐藏，Shift+Delete）② 覆盖替换
    /// ③ **追加用户库独有条目**（自造词/覆盖词不在基本库组 → 随查询结果显示）。
    /// `code` 为本次查询键（exact 用，含空组场景）；prefix/initial_top 传 ""——
    /// 跨 code 场景独有条目不做（低频路径，v1 取舍）。
    fn merged(&self, code: &str, mut entries: Vec<Entry>) -> Vec<Entry> {
        let Some(user) = self.user() else {
            return entries;
        };
        // ① 屏蔽过滤（仅作用于基本库条目——隐藏语义：先删用户库、再屏蔽基础库）
        entries.retain(|e| !user.is_blocked(&e.code, &e.word));
        // ③ 独有条目：查询 code 组中用户库有而基本库组没有的词条（被屏蔽的跳过）
        if !code.is_empty() {
            for (word, adj) in user.adjusted(code) {
                if user.is_blocked(code, word) {
                    continue;
                }
                if !entries.iter().any(|e| e.code == code && e.word == *word) {
                    entries.push(Entry {
                        word: word.clone(),
                        code: code.to_string(),
                        weight: *adj,
                    });
                }
            }
        }
        // ② 覆盖替换（按各自 code 查，prefix/initial_top 跨 code 场景同样生效）
        for e in &mut entries {
            if let Some((_, adj)) = user.adjusted(&e.code).iter().find(|(w, _)| w == &e.word) {
                e.weight = *adj;
            }
        }
        entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
        entries
    }

    // ===== 内部：索引二分 / 记录物化（视图已校验，索引直接）=====

    /// 第一个 code >= target 的索引位置（二分）。
    fn lower_bound(&self, target: &[u8]) -> usize {
        let n = self.index.len() / 4;
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.code_at(self.index_off(mid)) < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// 第一个 code > target 的索引位置（二分）。
    fn upper_bound(&self, target: &[u8]) -> usize {
        let n = self.index.len() / 4;
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.code_at(self.index_off(mid)) <= target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn index_off(&self, i: usize) -> usize {
        u32_at(&self.file.as_bytes(), self.index.start + i * 4) as usize
    }

    /// 记录 code 字节（相对记录体段起点）。
    fn code_at(&self, rel: usize) -> &[u8] {
        let b = &self.file.as_bytes()[self.records.start + rel..];
        let len = b[0] as usize;
        &b[1..1 + len]
    }

    fn entry_at(&self, rel: usize) -> Entry {
        self.entry_at_with_step(self.records.start + rel).0
    }

    /// 物化记录（绝对文件偏移）+ 下一条记录偏移（桶顺序遍历用）。
    fn entry_at_with_step(&self, abs: usize) -> (Entry, usize) {
        let b = &self.file.as_bytes()[abs..];
        let code_len = b[0] as usize;
        let code = std::str::from_utf8(&b[1..1 + code_len])
            .expect("词库已校验 code 边界")
            .to_string();
        let word_len = u16::from_le_bytes([b[1 + code_len], b[2 + code_len]]) as usize;
        let word_start = 3 + code_len;
        let word = std::str::from_utf8(&b[word_start..word_start + word_len])
            .expect("词库已校验 word 边界")
            .to_string();
        let w_off = word_start + word_len;
        let weight = u32::from_le_bytes([b[w_off], b[w_off + 1], b[w_off + 2], b[w_off + 3]]);
        (Entry { word, code, weight }, abs + w_off + 4)
    }
}

/// 段表查找指定类型段。
fn seg_of(segs: &[(u8, Range<usize>)], ty: u8) -> Option<Range<usize>> {
    segs.iter().find(|(t, _)| *t == ty).map(|(_, r)| r.clone())
}

/// 单条记录从当前位置起的字节长度（越界 → Err，消息含细节）。
/// 记录：u8 code_len | code | u16 word_len | word | u32 weight。
fn record_step(rest: &[u8]) -> Result<usize, String> {
    if rest.len() < 1 {
        return Err("记录体截断（缺 code_len）".into());
    }
    let code_len = rest[0] as usize;
    if 1 + code_len + 2 + 4 > rest.len() {
        return Err("记录体截断（code/word/weight 越界）".into());
    }
    let word_len = u16::from_le_bytes([rest[1 + code_len], rest[2 + code_len]]) as usize;
    if 1 + code_len + 2 + word_len + 4 > rest.len() {
        return Err("记录体截断（word 越界）".into());
    }
    Ok(1 + code_len + 2 + word_len + 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Dict {
        Dict::from_entries(vec![
            ("nihao".into(), "你好".into(), 8000),
            ("nihao".into(), "泥嚎".into(), 100),
            ("nihao".into(), "你好".into(), 1),
            ("de".into(), "的".into(), 100000),
            ("de".into(), "得".into(), 300),
            ("de".into(), "地".into(), 200),
            ("xian".into(), "先".into(), 500),
            ("xi'an".into(), "西安".into(), 50),
            ("abc".into(), "ABC".into(), 10),
        ])
    }

    #[test]
    fn exact_sorted_and_deduped() {
        let d = sample();
        let de = d.exact("de");
        assert_eq!(de.len(), 3);
        assert_eq!(de[0].word, "的");
        assert_eq!(de[1].word, "得");
        assert_eq!(de[2].weight, 200);
        assert_eq!(d.exact("nihao").len(), 2);
        assert_eq!(d.exact("nihao")[0].weight, 8000);
    }

    #[test]
    fn prefix_cross_code() {
        let d = sample();
        let hits = d.prefix("nih", 10);
        assert!(hits.iter().any(|e| e.word == "你好"));
        assert_eq!(d.prefix("nihao", 10).len(), 0);
        assert_eq!(d.prefix("", 10).len(), 0);
    }

    #[test]
    fn totals_and_syllables() {
        let d = sample();
        assert_eq!(
            d.total_weight(),
            100000 + 300 + 200 + 8000 + 100 + 500 + 50 + 10
        );
        assert_eq!(d.entry_count(), 8); // "你好" 重复条目去重
        assert!(d.syllables().contains("ni"));
        assert!(d.syllables().contains("hao"));
        assert!(d.syllables().contains("xian"));
        assert!(!d.syllables().contains("abc"));
        assert_eq!(d.max_word_syllables(), 2);
    }

    #[test]
    fn initial_bucket_top_by_weight() {
        let d = Dict::from_entries(vec![
            ("de".into(), "的".into(), 100000),
            ("de".into(), "得".into(), 300),
            ("da".into(), "大".into(), 5000),
            ("dan".into(), "但".into(), 4000),
            ("di".into(), "地".into(), 20000),
            ("bu".into(), "不".into(), 30000),
            ("zhongguo".into(), "中国".into(), 90000),
            ("zhong".into(), "中".into(), 100),
        ]);
        // d 桶：词频降序（的/地/大/但/得），截断生效
        let top = d.initial_top('d', 10);
        let words: Vec<&str> = top.iter().map(|e| e.word.as_str()).collect();
        assert_eq!(words, vec!["的", "地", "大", "但", "得"]);
        // limit 截断
        assert_eq!(d.initial_top('d', 2).len(), 2);
        // 无该字母 → 空
        assert!(d.initial_top('q', 10).is_empty());
        // 大写不入桶
        assert!(d.initial_top('A', 10).is_empty());
        // 桶只收单字：多字词"中国"不入桶；z 桶只含"中"
        let ztop = d.initial_top('z', 10);
        let z: Vec<&str> = ztop.iter().map(|e| e.word.as_str()).collect();
        assert_eq!(z, vec!["中"]);
        assert!(!z.contains(&"中国"), "多字词不应入桶，实际：{z:?}");
    }

    #[test]
    fn bucket_cap_truncates_to_constant() {
        // 灌入超过 INITIAL_BUCKET_SIZE 的同首字母单字，验证截断
        let mut items = Vec::new();
        for i in 0..(INITIAL_BUCKET_SIZE + 100) {
            let w = char::from_u32(0x4e00 + (i % 2000) as u32)
                .unwrap()
                .to_string();
            items.push((format!("b{}", i % 50), w, i as u32));
        }
        // 多字词不入桶：再加一批 2 字词验证不占桶位
        for i in 0..50 {
            items.push((format!("b{}", i % 50), format!("词语{i}"), 999_999_999));
        }
        let d = Dict::from_entries(items);
        let top = d.initial_top('b', usize::MAX);
        assert_eq!(top.len(), INITIAL_BUCKET_SIZE);
        assert!(
            top.iter().all(|e| e.word.chars().count() == 1),
            "桶应只含单字"
        );
    }

    #[test]
    fn default_dict_empty() {
        let d = Dict::default();
        assert_eq!(d.entry_count(), 0);
        assert!(d.exact("nihao").is_empty());
        assert!(d.initial_top('a', 10).is_empty());
        assert_eq!(d.total_weight(), 0);
    }

    #[test]
    fn user_overlay_merges_into_queries() {
        let d = sample();
        // 未装配：原始序（的/得/地）
        let de = d.exact("de");
        assert_eq!(de[0].word, "的");
        assert_eq!(de[1].word, "得");
        // 装配覆盖：得 ↔ 地 交换（得升到首位、地降到最后）
        let user = crate::userdict::UserDict::empty()
            .apply_swap("de", "的", 200, "de", "得", 100000)
            .apply_swap("de", "地", 0, "de", "的", 200);
        d.set_user(Arc::new(user));
        let de = d.exact("de");
        let words: Vec<&str> = de.iter().map(|e| e.word.as_str()).collect();
        assert_eq!(words, vec!["得", "的", "地"]);
        assert_eq!(de[0].weight, 100000);
        // effective_weight：覆盖值优先、未覆盖回基本库、不存在词条为 None
        assert_eq!(d.effective_weight("de", "得"), Some(100000));
        assert_eq!(d.effective_weight("de", "地"), Some(0));
        assert_eq!(d.effective_weight("de", "的"), Some(200));
        assert_eq!(d.effective_weight("nihao", "你好"), Some(8000));
        assert_eq!(d.effective_weight("nihao", "不存在词"), None);
        // exact_single 继承 merge（单段档路径）
        let d2 = Dict::from_entries(vec![
            ("de".into(), "的".into(), 100),
            ("de".into(), "得".into(), 50),
        ]);
        let user = crate::userdict::UserDict::empty().apply_swap("de", "的", 10, "de", "得", 200);
        d2.set_user(Arc::new(user));
        assert_eq!(d2.exact_single("de")[0].word, "得");
        // prefix 同样 merge（低频联想路径）：同 code 相邻交换后跨 code 全局排序
        let d3 = Dict::from_entries(vec![
            ("nima".into(), "尼玛".into(), 10),
            ("nima".into(), "你吗".into(), 5),
            ("nihao".into(), "你好".into(), 100),
            ("nish".into(), "你失".into(), 50),
        ]);
        let user =
            crate::userdict::UserDict::empty().apply_swap("nima", "尼玛", 5, "nima", "你吗", 1000);
        d3.set_user(Arc::new(user));
        let hits = d3.prefix("ni", 10);
        assert_eq!(hits[0].word, "你吗", "覆盖后权重领先");
        // 稳定排序：同 weight 保持基本库原序
        let d4 = Dict::from_entries(vec![
            ("abc".into(), "A".into(), 500),
            ("abc".into(), "B".into(), 500),
        ]);
        d4.set_user(Arc::new(
            crate::userdict::UserDict::empty().apply_swap("abc", "A", 500, "abc", "B", 500),
        ));
        let e4 = d4.exact("abc");
        assert_eq!(e4[0].word, "A");
        assert_eq!(e4[1].word, "B");
    }

    #[test]
    fn user_swap_roundtrip_through_dict() {
        // 端到端：swap（写盘）→ 新 Dict 装配加载 → merge 一致（跨进程持久化语义）
        let dir = std::env::temp_dir();
        let path = dir.join(format!("iuv-dict-user-swap-{}.imedic", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let d = sample();
        let user =
            crate::userdict::UserDict::empty().apply_swap("de", "的", 100, "de", "得", 100000);
        user.save(&path).unwrap();
        let loaded = crate::userdict::UserDict::load(&path).unwrap();
        d.set_user(Arc::new(loaded));
        let de = d.exact("de");
        assert_eq!(de[0].word, "得");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn user_unique_entries_append_to_queries() {
        // M2 自造词：词不在基本库 → merged 追加显示（zhangweiwei 逐字选后，再打整串直接出词）
        let d = Dict::from_entries(vec![
            ("zhang".into(), "张".into(), 90000),
            ("zhang'wei'wei".into(), "张威威".into(), 6000),
            ("zhang'wei'wei".into(), "张薇薇".into(), 4000),
        ]);
        let user = crate::userdict::UserDict::empty()
            .set_entry("zhang'wei'wei", "张葳葳", 5000)
            .set_entry("wei", "葳", 50);
        d.set_user(Arc::new(user));
        // exact：基本库 2 条 + 独有 1 条 = 3 条，按权重排序（张葳葳 5000 居中）
        let hits = d.exact("zhang'wei'wei");
        let texts: Vec<&str> = hits.iter().map(|e| e.word.as_str()).collect();
        assert_eq!(texts, vec!["张威威", "张葳葳", "张薇薇"]);
        assert_eq!(d.effective_weight("zhang'wei'wei", "张葳葳"), Some(5000));
        // 单字组（wei）的独有条目同样追加
        let wei = d.exact("wei");
        assert!(wei.iter().any(|e| e.word == "葳"));
        // 未装配用户库时查询不受影响
        let d2 = Dict::from_entries(vec![("zhang'wei'wei".into(), "张威威".into(), 6000)]);
        assert_eq!(d2.exact("zhang'wei'wei").len(), 1);
    }

    #[test]
    fn user_block_hides_base_entries() {
        // M2 隐藏：屏蔽基础库词条（Shift+Delete），查询剔除；覆盖+屏蔽叠加时屏蔽优先
        let d = Dict::from_entries(vec![
            ("shou'xuan".into(), "首选".into(), 8000),
            ("shou'xuan".into(), "手癣".into(), 300),
            ("shou'xuan".into(), "手选".into(), 100),
        ]);
        let user = crate::userdict::UserDict::empty().block("shou'xuan", "手癣");
        d.set_user(Arc::new(user));
        let hits = d.exact("shou'xuan");
        let texts: Vec<&str> = hits.iter().map(|e| e.word.as_str()).collect();
        assert_eq!(texts, vec!["首选", "手选"], "手癣被屏蔽，其余保序");
        // effective_weight 语义：词仍存在（隐藏优先级在展示层）
        assert_eq!(d.effective_weight("shou'xuan", "手癣"), Some(300));
        // 屏蔽 + 覆盖叠加：被屏蔽的条目即使有覆盖也不出现
        let user = crate::userdict::UserDict::empty()
            .block("shou'xuan", "手癣")
            .set_entry("shou'xuan", "手癣", 99999);
        d.set_user(Arc::new(user));
        assert!(!d.exact("shou'xuan").iter().any(|e| e.word == "手癣"));
    }
}
