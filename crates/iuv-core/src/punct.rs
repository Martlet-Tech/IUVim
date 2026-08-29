//! 中文标点映射（会话外中文模式直接上屏全角标点）。
//!
//! 键位基准 = 微软拼音中文标点键位表，
//! 微软表未列的键补搜狗常用全角形（`[`/`]`/`{`/`}`/`~`/`#`/`%`/`*`/`=`/`+`/`|`）。
//! 顿号在 `\` 键（微软惯例）；书名号固定方向（`<`→`《`、`>`→`》`，搜狗习惯）。
//! 引号 `'`/`"` 自动配对（开/关交替），配对状态由调用方（TSF）持有并传入。

use crate::config::WidthMode;

/// ASCII 标点 → 中文全角标点。未命中返回 None（该字符直通给应用）。
///
/// `quote_open`：引号配对状态。`'`/`"` 命中时按状态返回开/关形
/// （`quote_open=true` → `‘`/`“`，`false` → `’`/`”`），其余字符忽略该参数。
/// 双符输出（省略号 `……`、破折号 `——`）返回双字符字符串。
pub fn chinese_punct(ascii: char, quote_open: bool) -> Option<&'static str> {
    Some(match ascii {
        // 微软拼音中文标点键位表
        ',' => "，",
        '.' => "。",
        ';' => "；",
        ':' => "：",
        '?' => "？",
        '!' => "！",
        '(' => "（",
        ')' => "）",
        '<' => "《",
        '>' => "》",
        '^' => "……",
        '-' => "——",
        '\\' => "、",
        '@' => "·",
        '&' => "—",
        '$' => "￥",
        // 引号自动配对（开/关交替）
        '\'' => if quote_open { "‘" } else { "’" },
        '"' => if quote_open { "“" } else { "”" },
        // 搜狗补充键（微软表未列）
        '`' => "～",
        '~' => "～",
        '#' => "＃",
        '%' => "％",
        '*' => "＊",
        '[' => "【",
        ']' => "】",
        '{' => "『",
        '}' => "』",
        '=' => "＝",
        '+' => "＋",
        '|' => "｜",
        // 其余 ASCII 标点直通（`/`、`_`、`1`…）
        _ => return None,
    })
}

/// US 布局 Shift 推导：无 Shift 字符 → 有 Shift 字符（`;`→`:`、`1`→`!`…）。
/// 命中返回 Shift 后字符；未命中返回原字符（非 US 布局优雅降级，不误吞）。
pub fn shifted_punct(base: char, shift: bool) -> char {
    if !shift {
        return base;
    }
    match base {
        '`' => '~',
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        c => c,
    }
}

/// ASCII → 全角（U+FF01..U+FF5E 标准映射，28-initial-state-settings.md 全角行为）。
///
/// - `a-z` → `ａ-ｚ`（U+FF41 起）、`A-Z` → `Ａ-Ｚ`（U+FF21 起）、`0-9` → `０-９`（U+FF10 起）
/// - `0x21..=0x7E` 符号 → 原值 `+0xFEE0`（全区间无例外：`/`→`／`、`[`→`［`、`~`→`～`…）
/// - 空格 → `U+3000`（全角空格，微软全角模式对齐）
/// - 非 ASCII / 控制字符 → None（不转换，直通给应用）
pub fn fullwidth(c: char) -> Option<char> {
    if c == ' ' {
        return Some('\u{3000}');
    }
    let code = c as u32;
    let fw = match c {
        'a'..='z' => 0xFF41 + (code - 'a' as u32),
        'A'..='Z' => 0xFF21 + (code - 'A' as u32),
        '0'..='9' => 0xFF10 + (code - '0' as u32),
        c if (0x21..=0x7E).contains(&(c as u32)) => code + 0xFEE0,
        _ => return None,
    };
    char::from_u32(fw)
}

/// 提交文本宽度转换（预编辑原文上屏，28-initial-state-settings.md §8 影响点 1）：
/// `width == Full` 时逐字符套 `fullwidth`（汉字/全角字符不受影响原样保留）；
/// `Half` 时原样返回。覆盖 Enter/无候选空格/flush/原文兜底候选等所有原文上屏路径。
pub fn fullwidth_text(text: &str, width: WidthMode) -> String {
    if width != WidthMode::Full {
        return text.to_string();
    }
    text.chars()
        .map(|c| fullwidth(c).unwrap_or(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msime_table_full_mapping() {
        let q = true;
        let cases = [
            (',', "，"),
            ('.', "。"),
            (';', "；"),
            (':', "："),
            ('?', "？"),
            ('!', "！"),
            ('(', "（"),
            (')', "）"),
            ('<', "《"),
            ('>', "》"),
            ('^', "……"),
            ('-', "——"),
            ('\\', "、"),
            ('@', "·"),
            ('&', "—"),
            ('$', "￥"),
        ];
        for (ascii, expect) in cases {
            assert_eq!(chinese_punct(ascii, q), Some(expect), "键位 {ascii:?}");
        }
    }

    #[test]
    fn sogou_extras_mapping() {
        let q = true;
        let cases = [
            ('`', "～"),
            ('~', "～"),
            ('#', "＃"),
            ('%', "％"),
            ('*', "＊"),
            ('[', "【"),
            (']', "】"),
            ('{', "『"),
            ('}', "』"),
            ('=', "＝"),
            ('+', "＋"),
            ('|', "｜"),
        ];
        for (ascii, expect) in cases {
            assert_eq!(chinese_punct(ascii, q), Some(expect), "键位 {ascii:?}");
        }
    }

    #[test]
    fn quotes_pair_open_close() {
        // 开形
        assert_eq!(chinese_punct('\'', true), Some("‘"));
        assert_eq!(chinese_punct('"', true), Some("“"));
        // 关形
        assert_eq!(chinese_punct('\'', false), Some("’"));
        assert_eq!(chinese_punct('"', false), Some("”"));
        // 非引号字符不受 quote_open 影响
        assert_eq!(chinese_punct(',', true), Some("，"));
        assert_eq!(chinese_punct(',', false), Some("，"));
    }

    #[test]
    fn unmapped_chars_release() {
        // 微软表未列且搜狗补表未收 → 直通给应用
        assert_eq!(chinese_punct('/', true), None);
        assert_eq!(chinese_punct('_', true), None);
        assert_eq!(chinese_punct('1', true), None);
        assert_eq!(chinese_punct('a', true), None);
        assert_eq!(chinese_punct(' ', true), None);
        assert_eq!(chinese_punct('0', false), None);
    }

    #[test]
    fn shift_derivation() {
        // 无 Shift：原样
        assert_eq!(shifted_punct(';', false), ';');
        assert_eq!(shifted_punct('1', false), '1');
        assert_eq!(shifted_punct(',', false), ',');
        // Shift：US 布局符号
        assert_eq!(shifted_punct(';', true), ':');
        assert_eq!(shifted_punct('1', true), '!');
        assert_eq!(shifted_punct('2', true), '@');
        assert_eq!(shifted_punct('4', true), '$');
        assert_eq!(shifted_punct('6', true), '^');
        assert_eq!(shifted_punct('9', true), '(');
        assert_eq!(shifted_punct('0', true), ')');
        assert_eq!(shifted_punct('-', true), '_');
        assert_eq!(shifted_punct('=', true), '+');
        assert_eq!(shifted_punct('[', true), '{');
        assert_eq!(shifted_punct(']', true), '}');
        assert_eq!(shifted_punct('\\', true), '|');
        assert_eq!(shifted_punct('\'', true), '"');
        assert_eq!(shifted_punct(',', true), '<');
        assert_eq!(shifted_punct('.', true), '>');
        assert_eq!(shifted_punct('/', true), '?');
        // 非符号键（字母）Shift 后原样（大写由 TSF 处理，此处只管标点）
        assert_eq!(shifted_punct('a', true), 'a');
        assert_eq!(shifted_punct(' ', true), ' ');
    }

    #[test]
    fn quote_then_punct_cycle() {
        // 模拟配对状态流转：开 → 关 → 开
        let mut open = true;
        assert_eq!(chinese_punct('"', open), Some("“"));
        open = !open;
        assert_eq!(chinese_punct('"', open), Some("”"));
        open = !open;
        assert_eq!(chinese_punct('"', open), Some("“"));
    }

    #[test]
    fn fullwidth_letters() {
        // 小写 a-z → ａ-ｚ（U+FF41 起）
        assert_eq!(fullwidth('a'), Some('ａ'));
        assert_eq!(fullwidth('z'), Some('ｚ'));
        // 大写 A-Z → Ａ-Ｚ（U+FF21 起）
        assert_eq!(fullwidth('A'), Some('Ａ'));
        assert_eq!(fullwidth('Z'), Some('Ｚ'));
    }

    #[test]
    fn fullwidth_digits() {
        assert_eq!(fullwidth('0'), Some('０'));
        assert_eq!(fullwidth('5'), Some('５'));
        assert_eq!(fullwidth('9'), Some('９'));
    }

    #[test]
    fn fullwidth_symbols_and_space() {
        // 0x21..=0x7E → +0xFEE0 全区间无例外
        assert_eq!(fullwidth('!'), Some('！'));
        assert_eq!(fullwidth('/'), Some('／'));
        assert_eq!(fullwidth('['), Some('［'));
        assert_eq!(fullwidth(']'), Some('］'));
        assert_eq!(fullwidth('\\'), Some('＼'));
        assert_eq!(fullwidth('~'), Some('～'));
        assert_eq!(fullwidth('.'), Some('．'));
        assert_eq!(fullwidth('_'), Some('＿'));
        // 空格 → U+3000 全角空格
        assert_eq!(fullwidth(' '), Some('\u{3000}'));
        // 边界：0x21 与 0x7E
        assert_eq!(fullwidth(char::from_u32(0x21).unwrap()), Some('！'));
        assert_eq!(fullwidth(char::from_u32(0x7E).unwrap()), Some('～'));
    }

    #[test]
    fn fullwidth_unconvertible_release() {
        // 非 ASCII（汉字/全角已有字符）不转换
        assert_eq!(fullwidth('中'), None);
        assert_eq!(fullwidth('，'), None);
        assert_eq!(fullwidth('ａ'), None);
        // 控制字符不转换
        assert_eq!(fullwidth('\t'), None);
        assert_eq!(fullwidth('\n'), None);
        assert_eq!(fullwidth('\u{0}'), None);
    }

    #[test]
    fn fullwidth_text_converts_ascii_keeps_cjk() {
        let full = WidthMode::Full;
        let half = WidthMode::Half;
        // 全角：纯拼音 → 全角
        assert_eq!(fullwidth_text("nihao", full), "ｎｉｈａｏ");
        assert_eq!(fullwidth_text("hello world", full), "ｈｅｌｌｏ\u{3000}ｗｏｒｌｄ");
        // 全角：汉字/中文标点不受影响，拼音转
        assert_eq!(fullwidth_text("你好nihao", full), "你好ｎｉｈａｏ");
        assert_eq!(fullwidth_text("，nihao", full), "，ｎｉｈａｏ");
        // 全角：已全角字符幂等（不二次转换）
        assert_eq!(fullwidth_text("ａｂｃ", full), "ａｂｃ");
        // 半角：原样返回（含混合）
        assert_eq!(fullwidth_text("nihao", half), "nihao");
        assert_eq!(fullwidth_text("你好nihao", half), "你好nihao");
        // 空串
        assert_eq!(fullwidth_text("", full), "");
    }
}