//! 中文标点映射（会话外中文模式直接上屏全角标点）。
//!
//! 键位基准 = 微软拼音中文标点键位表（`docs/plan/xx-chinese-punct.md`），
//! 微软表未列的键补搜狗常用全角形（`[`/`]`/`{`/`}`/`~`/`#`/`%`/`*`/`=`/`+`/`|`）。
//! 顿号在 `\` 键（微软惯例）；书名号固定方向（`<`→`《`、`>`→`》`，搜狗习惯）。
//! 引号 `'`/`"` 自动配对（开/关交替），配对状态由调用方（TSF）持有并传入。

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
}