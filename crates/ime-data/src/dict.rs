//! Dict 查询层：二进制词库加载后的内存表示与查询。
//! W0 完整实现，冻结。任何签名变更需回契约 01-contract.md §3 修改。

use std::collections::{BTreeMap, BTreeSet};

/// 词条。`code` 为 squashed 全拼（无空格全小写），与查询键同形。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub word: String,
    pub code: String,
    pub weight: u32,
}

/// 内存词典：BTreeMap<squashed_code, Vec<Entry>>，每组按 weight 降序。
#[derive(Clone, Debug, Default)]
pub struct Dict {
    map: BTreeMap<String, Vec<Entry>>,
    total: u64,
    entry_count: usize,
    max_word_syllables: usize,
    syllables: BTreeSet<String>,
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
    "hang", "hao", "he", "hei", "hen", "heng", "hong", "hou", "hu", "hua", "huai", "huan",
    "huang", "hui", "hun", "huo", "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing",
    "jiong", "jiu", "ju", "juan", "jue", "jun", "ka", "kai", "kan", "kang", "kao", "ke", "ken",
    "keng", "kong", "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo", "la",
    "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao",
    "lie", "lin", "ling", "liu", "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve",
    "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie",
    "min", "ming", "miu", "mo", "mou", "mu", "na", "nai", "nan", "nang", "nao", "ne", "nei",
    "nen", "neng", "ni", "nian", "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou",
    "nu", "nuan", "nuo", "nv", "nve", "o", "ou", "pa", "pai", "pan", "pang", "pao", "pei", "pen",
    "peng", "pi", "pian", "piao", "pie", "pin", "ping", "po", "pou", "pu", "qi", "qia", "qian",
    "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan", "que", "qun", "ran",
    "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "ruan", "rui", "run", "ruo",
    "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha", "shai", "shan", "shang",
    "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai", "shuan",
    "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun", "suo",
    "ta", "tai", "tan", "tang", "tao", "te", "teng", "ti", "tian", "tiao", "tie", "ting", "tong",
    "tou", "tu", "tuan", "tui", "tun", "tuo", "wa", "wai", "wan", "wang", "wei", "wen", "weng",
    "wo", "wu", "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu",
    "xu", "xuan", "xue", "xun", "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo",
    "yong", "you", "yu", "yuan", "yue", "yun", "za", "zai", "zan", "zang", "zao", "ze", "zei",
    "zen", "zeng", "zha", "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng",
    "zhi", "zhong", "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo",
    "zi", "zong", "zou", "zu", "zuan", "zui", "zun", "zuo",
];

/// 判定一段字符串是否为标准合法音节（二分查找）。
pub(crate) fn is_syllable(s: &str) -> bool {
    SYLLABLES.binary_search(&s).is_ok()
}

/// 贪心最长匹配切分（与 Quanpin 同一规则）：返回 (音节序列, 音节数)。
/// `'` 为强制分隔（不产生段）；匹配失败的单字母原样保留，保证对任意输入不 panic。
fn greedy_segment(code: &str) -> Vec<String> {
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

impl Dict {
    /// 测试/用户词库构造器。items = (squashed_code, word, weight)。
    /// 同码多条按 weight 降序归并；同 (code,word) 去重取最大 weight。
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
        let mut total = 0u64;
        let mut entry_count = 0usize;
        let mut max_word_syllables = 0usize;
        let mut syllables = BTreeSet::new();
        for group in map.values_mut() {
            group.sort_by(|a, b| b.weight.cmp(&a.weight));
            entry_count += group.len();
            total += group.iter().map(|e| e.weight as u64).sum::<u64>();
            let seg = greedy_segment(&group[0].code);
            // 仅全为合法音节的词条计入词长（英文条目如 "abc" 不算拼音词）
            if seg.iter().all(|s| is_syllable(s)) {
                max_word_syllables = max_word_syllables.max(seg.len());
            }
            for s in &seg {
                if is_syllable(s) {
                    syllables.insert(s.clone());
                }
            }
        }
        Dict { map, total, entry_count, max_word_syllables, syllables }
    }

    /// 精确查询：squashed_code 如 "nihao"。返回按 weight 降序切片。
    pub fn exact(&self, squashed_code: &str) -> &[Entry] {
        self.map.get(squashed_code).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 前缀补全：返回 squashed 以 prefix 开头（且不等于 prefix）的词条，
    /// 跨编码按 weight 降序，最多 limit 条。
    pub fn prefix(&self, squashed_prefix: &str, limit: usize) -> Vec<&Entry> {
        if limit == 0 || squashed_prefix.is_empty() {
            return Vec::new();
        }
        let mut all: Vec<&Entry> = Vec::new();
        for (code, group) in self.map.range(squashed_prefix.to_string()..) {
            if !code.starts_with(squashed_prefix) {
                break;
            }
            if code == squashed_prefix {
                continue;
            }
            all.extend(group.iter());
        }
        all.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
        all.truncate(limit);
        all
    }

    /// 全部音节集合（从所有 code 切出），供全拼切分器构造。
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
        assert_eq!(d.total_weight(), 100000 + 300 + 200 + 8000 + 100 + 500 + 50 + 10);
        assert_eq!(d.entry_count(), 8); // "你好" 重复条目去重
        assert!(d.syllables().contains("ni"));
        assert!(d.syllables().contains("hao"));
        assert!(d.syllables().contains("xian"));
        assert!(!d.syllables().contains("abc"));
        assert_eq!(d.max_word_syllables(), 2);
    }
}
