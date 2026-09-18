//! Dict 查询层：IMEDIC02 平面词库的 mmap 零加工查询。
//! 加载 = mmap + 段表定位 + 一次边界校验扫描；查询 = 索引段二分 + 记录体物化。
//! 接口契约 01-contract.md §3（`exact` 系列返回物化 `Vec<Entry>`）。

use crate::format::{
    self, greedy_join, reachable_split, FILE_HEADER_LEN, MAGIC, SEG_BUCKETS, SEG_HEADER_LEN,
    SEG_INDEX, SEG_META, SEG_RECORDS, SEG_REVERSE,
};
use crate::mmap::MappedFile;
use crate::userdict::UserDict;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::ops::Range;
use std::sync::{Arc, Mutex};

/// 词条。`code` 为 squashed 全拼（无空格全小写，音节间 `'` 分隔），与查询键同形。
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// 段5 整跨词反查（46 号 §3.1，可选段；None = 旧词库无此段）
    reverse: Option<Range<usize>>,
    total: u64,
    entry_count: usize,
    max_word_syllables: usize,
    syllables: BTreeSet<String>,
    /// 用户库整跨词反查（46 号 §3.1：set_user 时从 cover_iter 派生，
    /// concat → 可达且非贪心码形的码表）。格式不动、daemon 不动，纯查询侧派生。
    user_reverse: Mutex<HashMap<String, Vec<String>>>,
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
            reverse: self.reverse.clone(),
            total: self.total,
            entry_count: self.entry_count,
            max_word_syllables: self.max_word_syllables,
            syllables: self.syllables.clone(),
            user_reverse: Mutex::new(
                self.user_reverse
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            ),
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
            reverse: None,
            total: 0,
            entry_count: 0,
            max_word_syllables: 0,
            syllables: BTreeSet::new(),
            user_reverse: Mutex::new(HashMap::new()),
            user: Mutex::new(None),
        }
    }
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
            let step = record_step(&bytes[pos..records.end]).map_err(&bad)?;
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
                let step = record_step(&bytes[pos..buckets.end]).map_err(&bad)?;
                pos += step;
            }
            count += 1;
        }
        if count != 26 {
            return Err(bad(format!("桶段应含 26 个桶，实际 {count}")));
        }

        // ---- 段5 整跨词反查（可选段，46 号 §3.1）：K | var_area_off | K×u32 键头偏移 | 键头区 | 变体区 ----
        let reverse = match seg_of(&segs, SEG_REVERSE) {
            Some(r) => {
                if r.len() < 8 {
                    return Err(bad("反查段过短".into()));
                }
                let k = u32_at(bytes, r.start) as usize;
                let var_area_off = u32_at(bytes, r.start + 4) as usize;
                if r.len() < 8 + k * 4 {
                    return Err(bad("反查段键偏移数组截断".into()));
                }
                if var_area_off < 8 + k * 4 || var_area_off > r.len() {
                    return Err(bad("反查段变体区偏移非法".into()));
                }
                for i in 0..k {
                    let off = u32_at(bytes, r.start + 8 + i * 4) as usize;
                    if off >= var_area_off {
                        return Err(bad("反查段键头偏移越界".into()));
                    }
                    let klen = bytes[r.start + off] as usize;
                    if off + 1 + klen + 4 + 2 > var_area_off {
                        return Err(bad("反查段键头越界".into()));
                    }
                }
                // 变体区逐条边界扫描（u8 code_len | code | u32 weight）
                let mut pos = r.start + var_area_off;
                while pos < r.end {
                    let cl = bytes[pos] as usize;
                    if pos + 1 + cl + 4 > r.end {
                        return Err(bad("反查段变体截断".into()));
                    }
                    pos += 1 + cl + 4;
                }
                Some(r)
            }
            None => None,
        };

        Ok(Dict {
            file: Arc::new(file),
            bucket_dir,
            index: index.clone(),
            records: records.clone(),
            buckets: buckets.clone(),
            reverse,
            total,
            entry_count,
            max_word_syllables,
            syllables,
            user_reverse: Mutex::new(HashMap::new()),
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

    /// 基础库精确查询（不过屏蔽/覆盖/独有条目——内部"词条真实存在性"语义；
    /// 外部一律走 exact 的叠加视图）。
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

    /// 零分配探针：是否存在 code == 目标的词条（39-rime-pipeline.md Step2：
    /// rime 核心游标走图用，热路径避免物化词条）。
    pub fn has_code(&self, squashed_code: &str) -> bool {
        if squashed_code.is_empty() {
            return false;
        }
        let target = squashed_code.as_bytes();
        let n = self.index.len() / 4;
        let base = if n == 0 {
            false
        } else {
            let lo = self.lower_bound(target);
            lo < n && self.code_at(self.index_off(lo)) == target
        };
        // 用户独有词条：基础库 mmap 无此码但仍应可收集（rime 游标探针可见性）
        base || self
            .user()
            .map(|u| u.has_code(squashed_code))
            .unwrap_or(false)
    }

    /// 零分配探针：是否存在以目标为真前缀（且不等长）的词条。
    /// 实现：二分定位第一个**大于**目标的码（upper_bound），再查一次前缀——
    /// 严格 O(log n)，不受等长码簇大小影响（"ni" 数百同码单字的实测教训）。
    pub fn has_prefix(&self, squashed_prefix: &str) -> bool {
        if squashed_prefix.is_empty() {
            return false;
        }
        let target = squashed_prefix.as_bytes();
        let n = self.index.len() / 4;
        if n == 0 {
            return false;
        }
        let mut lo = self.lower_bound(target);
        // 跳过与目标相等的码段（lower_bound 落点可能就在簇首）
        if lo < n && self.code_at(self.index_off(lo)) == target {
            let mut hi = n;
            while lo < hi {
                let mid = (lo + hi) / 2;
                if self.code_at(self.index_off(mid)) <= target {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
        }
        if lo >= n {
            return self
                .user()
                .map(|u| u.has_prefix(squashed_prefix))
                .unwrap_or(false);
        }
        let base = self.code_at(self.index_off(lo)).starts_with(target);
        base || self
            .user()
            .map(|u| u.has_prefix(squashed_prefix))
            .unwrap_or(false)
    }

    /// 前缀补全：返回 squashed 以 prefix 开头（且不等于 prefix）的词条，
    /// 跨编码按 weight 降序，最多 limit 条。实现为范围物化 + top-k 选择 + 权重
    /// 降序排序 + 截断——排序必须在截断**之前**（码序前 64/20 条 ≠ 高频前 64/20 条，
    /// cheng'y 的 64 截断曾把高频「成员」排挤到窗外，2026-08-29 λ 校准实测）。
    /// 无用户库时同样保证权重序（此前排序只在 merged 有用户库分支里发生，
    /// REPL/对拍与生产行为隐性分叉）。top-k 选择已于 46 号 §3.4 落地（原 TODO）。
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
        // 46 号 §3.4：top-k 选择替代全量排序（兑现本函数留档 TODO；语义不变）。
        // 顺序红线（8/29 校准教训）：**先 merged（屏蔽/调权/追加）后选优截断**，
        // 否则用户覆盖权重救不回已被截掉的词条。
        out = self.merged("", out);
        if out.len() > limit {
            out.select_nth_unstable_by(limit - 1, worse_first());
            out.truncate(limit);
        }
        out.sort_by(worse_first());
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

    /// 该串是否为完整合法音节（rime 与引擎公共路径使用，2026-08-26 去重）。
    pub fn is_syllable(&self, s: &str) -> bool {
        self.syllables.contains(s)
    }

    /// 该串是否为某音节的真前缀（微软对齐单段档判定）。
    pub fn is_syllable_prefix(&self, s: &str) -> bool {
        !s.is_empty() && self.syllables.iter().any(|syl| syl.starts_with(s))
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
    /// 46 号 §3.1：同处派生用户库整跨词反查（cover_iter → 可达且非贪心的码形，
    /// 按 concat 分组）——用户库格式与 daemon 均不动，纯查询侧派生索引。
    pub fn set_user(&self, user: Arc<UserDict>) {
        let mut code_adj: HashMap<&str, u32> = HashMap::new();
        for (code, _word, adj) in user.cover_iter() {
            code_adj
                .entry(code)
                .and_modify(|w| *w = (*w).max(adj))
                .or_insert(adj);
        }
        let mut pairs: Vec<(String, String, u32)> = code_adj
            .into_iter()
            .filter_map(|(code, adj)| {
                let segs = reachable_split(code, &self.syllables)?;
                let concat: String = segs.concat();
                if greedy_join(&concat, &self.syllables) == code {
                    return None; // 唯一变体 = 贪心码形：决策恒等贪心，死数据
                }
                Some((concat, code.to_string(), adj))
            })
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)).then(a.1.cmp(&b.1)));
        let mut rev: HashMap<String, Vec<String>> = HashMap::new();
        for (concat, code, _) in pairs {
            let slot = rev.entry(concat).or_default();
            if slot.last() != Some(&code) {
                slot.push(code);
            }
        }
        *self.user_reverse.lock().unwrap_or_else(|e| e.into_inner()) = rev;
        *self.user.lock().unwrap_or_else(|e| e.into_inner()) = Some(user);
    }

    /// 当前用户覆盖表（未装配 → None）。
    pub fn user(&self) -> Option<Arc<UserDict>> {
        self.user.lock().unwrap_or_else(|e| e.into_inner()).clone()
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

    // ===== 整跨词反查 + 词典游标（46 号 §3.1/§3.2）=====

    /// 整跨词反查：`concat` = raw 去撇号归一形；`seps` = 用户强制撇号在 concat
    /// 坐标系的字节偏移（升序）。返回分隔掩码 ⊇ seps 的码——基础库变体按构建期
    /// 权重降序在前，用户库独有码随后（有效权重由调用方经 `exact` 统一裁决）。
    /// 旧词库无反查段 → 恒空（引擎自然走贪心）。
    pub fn reverse_candidates(&self, concat: &str, seps: &[usize]) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(r) = &self.reverse {
            let bytes = self.file.as_bytes();
            let k = u32_at(bytes, r.start) as usize;
            let target = concat.as_bytes();
            let (mut lo, mut hi) = (0usize, k);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if self.reverse_key(r, mid).0 < target {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            if lo < k {
                let (key, var_off, var_count) = self.reverse_key(r, lo);
                if key == target {
                    // 变体区偏移为变体区相对；var_area_off 在段头第 2 个 u32
                    let var_area_off = u32_at(bytes, r.start + 4) as usize;
                    let base = r.start + var_area_off + var_off;
                    for j in 0..var_count {
                        let (code, _w) = self.reverse_var(base, j);
                        if seps_cover(&code, seps) {
                            out.push(code);
                        }
                    }
                }
            }
        }
        let urev = self.user_reverse.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(codes) = urev.get(concat) {
            for code in codes {
                if seps_cover(code, seps) && !out.contains(code) {
                    out.push(code.clone());
                }
            }
        }
        out
    }

    /// 第 i 个键头（键字节, 变体区相对偏移, 变体数）。
    /// 键头布局：u8 klen | key | u32 var_off | u16 var_count。
    fn reverse_key(&self, r: &Range<usize>, i: usize) -> (&[u8], usize, usize) {
        let bytes = self.file.as_bytes();
        let off = u32_at(bytes, r.start + 8 + i * 4) as usize;
        let h = r.start + off;
        let klen = bytes[h] as usize;
        let key = &bytes[h + 1..h + 1 + klen];
        let var_off = u32_at(bytes, h + 1 + klen) as usize;
        let var_count =
            u16::from_le_bytes([bytes[h + 1 + klen + 4], bytes[h + 1 + klen + 5]]) as usize;
        (key, var_off, var_count)
    }

    /// 变体区第 j 条（从 base 顺序走——变体定长前缀 + 变长 code，条数恒小）。
    fn reverse_var(&self, base: usize, j: usize) -> (String, u32) {
        let bytes = self.file.as_bytes();
        let mut pos = base;
        for _ in 0..j {
            pos += 1 + bytes[pos] as usize + 4;
        }
        let cl = bytes[pos] as usize;
        let code = std::str::from_utf8(&bytes[pos + 1..pos + 1 + cl])
            .expect("词库已校验反查变体 code 边界")
            .to_string();
        let w = u32::from_le_bytes([
            bytes[pos + 1 + cl],
            bytes[pos + 2 + cl],
            bytes[pos + 3 + cl],
            bytes[pos + 4 + cl],
        ]);
        (code, w)
    }

    // ===== 词典游标（46 号 §3.2，对齐 librime Table 游标语义）=====

    /// 根游标（空前缀 = 全表）。
    pub fn cursor(&self) -> DictCursor {
        DictCursor {
            lo: 0,
            hi: self.index.len() / 4,
            plen: 0,
        }
    }

    /// 从游标步进一个键片段：音节（join 键族传 `\'`+音节）、简拼字母（concat
    /// 键族直拼）、补全段字节均可——片段就是「追加到累计前缀后的字节」。
    /// 返回 None = 无码以新前缀开头（exhausted 剪枝）；步进内二分只在
    /// 父区间内收缩（越深越窄），比较用区间内码的 rest（`code[plen..]`），零分配。
    pub fn cursor_step(&self, c: DictCursor, seg: &[u8]) -> Option<DictCursor> {
        if c.lo >= c.hi {
            return None;
        }
        // lo'：区间内第一个 rest ≥ seg
        let (mut lo, mut hi) = (c.lo, c.hi);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.rest_at(mid, c.plen) < seg {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        // hi'：lo' 起第一个 rest 不以 seg 开头（同前缀码区在码序下连续）
        let mut a = lo;
        let mut b = c.hi;
        while a < b {
            let mid = (a + b) / 2;
            if self.rest_at(mid, c.plen).starts_with(seg) {
                a = mid + 1;
            } else {
                b = mid;
            }
        }
        if lo >= a {
            return None;
        }
        Some(DictCursor {
            lo,
            hi: a,
            plen: c.plen + seg.len(),
        })
    }

    /// 区间内存在码 == 累计前缀（等长码是区间内 lex 最小码，居首）。
    pub fn cursor_has_code(&self, c: DictCursor) -> bool {
        c.lo < c.hi && self.code_at(self.index_off(c.lo)).len() == c.plen
    }

    /// 区间内存在严格更长的码（BFS 可继续；区间内 lex 最大码即最长——
    /// 全部码共享前缀 plen，lex 更大者必更长）。
    pub fn cursor_deeper(&self, c: DictCursor) -> bool {
        c.lo < c.hi && self.code_at(self.index_off(c.hi - 1)).len() > c.plen
    }

    /// 物化等长码条目（≡ `exact`(累计前缀)）；码内 weight 降序由排序不变量
    /// 保证，叠加用户视图（屏蔽/调权/独有条目）后返回。
    pub fn cursor_exact(&self, c: DictCursor) -> Vec<Entry> {
        let mut out = Vec::new();
        for i in c.lo..c.hi {
            let e = self.entry_at(self.index_off(i));
            if e.code.len() != c.plen {
                break;
            }
            out.push(e);
        }
        let code = out.first().map(|e| e.code.clone()).unwrap_or_default();
        self.merged(&code, out)
    }

    /// 物化严格更长前缀条目（≡ `prefix`(累计前缀, limit)）。
    /// 46 号 §3.4：top-k 选择替代「全范围收集 + 全量排序」（语义不变；
    /// 顺序红线同 prefix：先 merged 后选优截断）。
    pub fn cursor_longer(&self, c: DictCursor, limit: usize) -> Vec<Entry> {
        if limit == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in c.lo..c.hi {
            let e = self.entry_at(self.index_off(i));
            if e.code.len() != c.plen {
                out.push(e);
            }
        }
        out = self.merged("", out);
        if out.len() > limit {
            out.select_nth_unstable_by(limit - 1, worse_first());
            out.truncate(limit);
        }
        out.sort_by(worse_first());
        out
    }

    /// 区间内码的 rest 切片（`code[plen..]`；区间不变量保证码长 ≥ plen）。
    fn rest_at(&self, i: usize, plen: usize) -> &[u8] {
        &self.code_at(self.index_off(i))[plen..]
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
        u32_at(self.file.as_bytes(), self.index.start + i * 4) as usize
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

/// 词典游标（46 号 §3.2）：码序区间 [lo, hi)（记录索引下标）+ 累计前缀字节长。
/// 区间语义 = 全部以「累计前缀」开头的码；前缀本身不存字符串——步进比较只用
/// 区间内码的 rest（`code[plen..]`），零分配零全局二分（对齐 librime Table 游标）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DictCursor {
    pub(crate) lo: usize,
    pub(crate) hi: usize,
    pub(crate) plen: usize,
}

/// 段表查找指定类型段。
fn seg_of(segs: &[(u8, Range<usize>)], ty: u8) -> Option<Range<usize>> {
    segs.iter().find(|(t, _)| *t == ty).map(|(_, r)| r.clone())
}

/// 候选优序比较器（更优在前）：weight 降序、word 升序。
fn worse_first() -> impl Fn(&Entry, &Entry) -> std::cmp::Ordering {
    |a: &Entry, b: &Entry| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word))
}

/// 码的撇号偏移集（concat 坐标）是否覆盖用户强制撇号集（46 号 §3.1 掩码过滤）。
fn seps_cover(code: &str, seps: &[usize]) -> bool {
    if seps.is_empty() {
        return true;
    }
    let mut code_seps = Vec::with_capacity(seps.len());
    let mut n = 0usize;
    for b in code.bytes() {
        if b == b'\'' {
            code_seps.push(n);
        } else {
            n += 1;
        }
    }
    seps.iter().all(|s| code_seps.contains(s))
}

/// 单条记录从当前位置起的字节长度（越界 → Err，消息含细节）。
/// 记录：u8 code_len | code | u16 word_len | word | u32 weight。
fn record_step(rest: &[u8]) -> Result<usize, String> {
    if rest.is_empty() {
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

    /// 46 号 §3.2：游标步进 / 等长命中 / 严格更长 / exhaustion / 等长码簇。
    #[test]
    fn cursor_walk_exact_and_deeper() {
        let d = Dict::from_entries(vec![
            ("ni".into(), "你".into(), 9000),
            ("ni'hao".into(), "你好".into(), 8000),
            ("ni'hao'a".into(), "你好啊".into(), 5),
            ("de".into(), "的".into(), 100000),
            ("de".into(), "得".into(), 300),
            ("de".into(), "地".into(), 200),
        ]);
        let root = d.cursor();
        let ni = d.cursor_step(root, b"ni").expect("ni 前缀应有码");
        assert!(d.cursor_has_code(ni), "单音节码 ni 等长命中");
        assert!(d.cursor_deeper(ni), "ni'hao/a 更长码存在");
        let nihao = d.cursor_step(ni, b"'hao").expect("ni'hao 应有码");
        assert!(d.cursor_has_code(nihao));
        let exact = d.cursor_exact(nihao);
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].word, "你好");
        assert!(d.cursor_deeper(nihao));
        // 长度-lex 交错（ni'hao'a 与 ni'hao）：子区间收缩正确
        let a = d.cursor_step(nihao, b"'a").expect("ni'hao'a 应有码");
        assert_eq!(d.cursor_exact(a)[0].word, "你好啊");
        // 等长码簇（de 的 的/得/地）与 exhaustion
        let de = d.cursor_step(root, b"de").expect("de 应有码");
        assert_eq!(d.cursor_exact(de).len(), 3);
        assert!(d.cursor_step(de, b"'z").is_none());
    }

    /// 46 号 §3.1：反查掩码过滤与贪心码形过滤（唯一可达码形 = 贪心 → 不入段）。
    #[test]
    fn reverse_candidates_mask_and_greedy_filter() {
        let d = sample();
        // concat "xian"：非贪心变体 xi'an（xian 即贪心码形，按 §3.1 过滤不入段）
        assert_eq!(d.reverse_candidates("xian", &[]), vec!["xi'an".to_string()]);
        // 用户强制撇号掩码：xi'an 的撇号在 concat 偏移 2
        assert_eq!(
            d.reverse_candidates("xian", &[2]),
            vec!["xi'an".to_string()]
        );
        // 掩码不满足：期望空
        assert!(d.reverse_candidates("xian", &[1]).is_empty());
        // 唯一可达码形 = 贪心（nihao）／码不可达（hahaha）→ 不入段
        assert!(d.reverse_candidates("nihao", &[]).is_empty());
        assert!(d.reverse_candidates("hahaha", &[]).is_empty());
    }

    /// 46 号 §3.1：用户库独有码形经 set_user 派生反查可达。
    #[test]
    fn user_reverse_candidates() {
        let d = sample();
        let user = crate::userdict::UserDict::empty().set_entry("xi'an'hao", "西安好", 100);
        d.set_user(Arc::new(user));
        let all = d.reverse_candidates("xianhao", &[]);
        assert!(all.contains(&"xi'an'hao".to_string()), "{all:?}");
        // 贪心码形仍被过滤
        assert!(!all.contains(&"xian'hao".to_string()));
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
        // 屏蔽 + 覆盖叠加：被屏蔽的条目即使有覆盖也不出现
        let user = crate::userdict::UserDict::empty()
            .block("shou'xuan", "手癣")
            .set_entry("shou'xuan", "手癣", 99999);
        d.set_user(Arc::new(user));
        assert!(!d.exact("shou'xuan").iter().any(|e| e.word == "手癣"));
    }

    #[test]
    fn prefix_weight_desc_without_user_dict() {
        // 前缀补全契约：跨编码 weight 降序，与是否装配用户库无关。
        // 回归钉（2026-08-29 λ 校准实测）：无用户库时曾按码序原样返回，
        // 64/20 截断把高频词排挤窗外（cheng'y 截断丢「成员」→ 整句误组）。
        let d = Dict::from_entries(vec![
            ("cheng'yuan".into(), "成员".into(), 10739),
            ("cheng'yi".into(), "乘以".into(), 2022),
            ("cheng'ya".into(), "承压".into(), 226),
        ]);
        let hits: Vec<String> = d
            .prefix("cheng'y", 2)
            .iter()
            .map(|e| e.word.clone())
            .collect();
        assert_eq!(hits, vec!["成员", "乘以"], "截断前必须先权重降序");
        // 装配用户库后契约不变（覆盖权重生效）
        let user = crate::userdict::UserDict::empty().set_entry("cheng'ya", "承压", 99999);
        d.set_user(Arc::new(user));
        let hits: Vec<String> = d
            .prefix("cheng'y", 1)
            .iter()
            .map(|e| e.word.clone())
            .collect();
        assert_eq!(hits, vec!["承压"], "用户覆盖后仍权重序");
    }
}
