//! ime-repl：CLI 调试前端（任务书 12-mod-ime-repl.md，Agent C 属主）。
//! 不注册输入法即可在终端验证引擎：交互式输入拼音看候选/选词，
//! 或 `--batch <拼音>` 打印全表候选供组装手册做冒烟断言。

use std::error::Error;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

use ime_core::{apply_keymap, Config, Effect, Engine, Key, Session, SessionEnd};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dict_path, batch_raw) = match parse_args(&args) {
        Some(x) => x,
        None => {
            print_usage();
            std::process::exit(1);
        }
    };
    let dict = ime_data::load(std::path::Path::new(&dict_path))
        .map_err(|e| format!("词典加载失败 {}：{}", dict_path, e))?;
    let engine = Engine::new(dict, Config::load());
    match batch_raw {
        Some(raw) => run_batch(&engine, &raw),
        None => interactive(&engine)?,
    }
    Ok(())
}

/// 命令行解析：`<dict.imedic>` 或 `<dict.imedic> --batch <拼音串>`。
fn parse_args(args: &[String]) -> Option<(String, Option<String>)> {
    match args {
        [dict] => Some((dict.clone(), None)),
        [dict, flag, raw] if flag == "--batch" => Some((dict.clone(), Some(raw.clone()))),
        _ => None,
    }
}

fn print_usage() {
    eprintln!("用法：ime-repl <dict.imedic>");
    eprintln!("       ime-repl <dict.imedic> --batch <拼音串>");
}

/// 交互模式：提示符 `>` 逐行读取（任务书 §3.2）。
fn interactive(engine: &Arc<Engine>) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut session: Option<Session> = None;
    loop {
        write!(stdout, "> ")?;
        stdout.flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break; // EOF
        }
        let line = line.trim();
        match line {
            "q" => break,
            "!" => dispatch(&mut session, Key::Esc),
            "" => dispatch(&mut session, Key::Space),
            _ => {
                if let Some(n) = digit(line) {
                    dispatch(&mut session, Key::Digit(n));
                } else if is_pinyin(line) {
                    // 拼音串：新建 Session，逐字符喂入后打印最终 Effect
                    let mut s = engine.start_session();
                    let mut e = s.effect();
                    for ch in line.chars() {
                        e = s.on_key(Key::Char(ch));
                    }
                    print_effect(&e);
                    session = Some(s);
                } else if let Some(ch) = single_char(line) {
                    // 标点键：按 keymap 重映射（默认 ,=上翻 . =下翻），与运行时一致
                    dispatch(&mut session, apply_keymap(Key::Char(ch), &engine.config().keymap));
                } else {
                    print_hint();
                }
            }
        }
    }
    Ok(())
}

/// 单字符（标点/快捷键输入），多字符返回 None。
fn single_char(line: &str) -> Option<char> {
    let mut it = line.chars();
    let c = it.next()?;
    if it.next().is_some() {
        None
    } else {
        Some(c)
    }
}

/// 对当前会话发一个键并刷新显示；会话结束则丢弃。
fn dispatch(session: &mut Option<Session>, key: Key) {
    let e = match session.as_mut() {
        Some(s) => s.on_key(key),
        None => return,
    };
    print_effect(&e);
    if e.end.is_some() {
        *session = None;
    }
}

/// 单字符 1-9 → Digit(n)。
fn digit(line: &str) -> Option<u8> {
    let b = line.as_bytes();
    match b {
        [c @ b'1'..=b'9'] => Some(c - b'0'),
        _ => None,
    }
}

/// 全小写字母 + `'` 构成的拼音串。
fn is_pinyin(line: &str) -> bool {
    !line.is_empty() && line.chars().all(|c| c.is_ascii_lowercase() || c == '\'')
}

fn print_hint() {
    eprintln!("提示：输入字母/`'` 串=拼音，空行=空格提交，1-9=选词，,=上翻 .=下翻，!=Esc，q=退出");
}

/// 打印一次按键后的 UI 快照（任务书 §3.2 格式）。
fn print_effect(e: &Effect) {
    if !e.reading.is_empty() {
        println!("{}", e.reading);
    }
    if !e.candidates.is_empty() {
        let items: Vec<String> = e
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mark = if i == e.selected { "*" } else { "" };
                format!("{}{}.{}", mark, i + 1, c.text)
            })
            .collect();
        println!(" {}", items.join(" "));
        println!(
            " [page {}/{} · total {}]",
            e.page.page + 1,
            e.page.page_count,
            e.page.total
        );
    }
    if let Some(word) = &e.part_commit {
        println!("< 部分上屏：{word}（尾巴续接）");
    }
    match &e.end {
        Some(SessionEnd::Commit(text)) => println!("< committed: {}", text),
        Some(SessionEnd::Cancel) => println!("< cancelled"),
        None => {}
    }
}

/// 批处理模式：打印 reading 行 + 全表候选（`序号<TAB>text<TAB>kind<TAB>weight`）。
fn run_batch(engine: &Arc<Engine>, raw: &str) {
    let e = collect_all(engine, raw);
    println!("{}", e.reading);
    for (i, c) in e.candidates.iter().enumerate() {
        println!("{}\t{}\t{:?}\t{}", i + 1, c.text, c.kind, c.weight);
    }
}

/// 输入 raw 后逐页翻到底，收集全表候选（page 信息仍为最后一页）。
fn collect_all(engine: &Arc<Engine>, raw: &str) -> Effect {
    let mut session = engine.start_session();
    for ch in raw.chars() {
        session.on_key(Key::Char(ch));
    }
    let mut e = session.effect();
    let mut all = e.candidates.clone();
    while e.page.page + 1 < e.page.page_count {
        e = session.on_key(Key::PageDown);
        all.extend(e.candidates.clone());
    }
    e.candidates = all;
    e
}

/// 逐键喂入新会话，会话结束（end 非 None）即停止。测试用（任务书 §4），bin 构建不引用。
#[cfg_attr(not(test), allow(dead_code))]
fn run_script(engine: &Arc<Engine>, keys: &[Key]) -> Vec<Effect> {
    let mut session = engine.start_session();
    let mut effects = Vec::with_capacity(keys.len());
    for &key in keys {
        let e = session.on_key(key);
        let ended = e.end.is_some();
        effects.push(e);
        if ended {
            break;
        }
    }
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use ime_data::Dict;

    fn engine_with(dict: Dict) -> Arc<Engine> {
        Engine::new(dict, Config::default())
    }

    /// 冒烟：输入"nihao"→空格提交首选"你好"（任务书 §4 要求）。
    #[test]
    fn input_then_space_commits_first_candidate() {
        let engine = engine_with(Dict::from_entries(vec![
            ("ni'hao".into(), "你好".into(), 8000),
            ("ni'hao".into(), "泥嚎".into(), 100),
        ]));
        let effects = run_script(
            &engine,
            &[
                Key::Char('n'),
                Key::Char('i'),
                Key::Char('h'),
                Key::Char('a'),
                Key::Char('o'),
                Key::Space,
            ],
        );
        assert_eq!(effects.len(), 6);
        assert_eq!(
            effects.last().unwrap().end,
            Some(SessionEnd::Commit("你好".into()))
        );
        let before = &effects[effects.len() - 2];
        assert_eq!(before.reading, "ni'hao");
        assert_eq!(before.candidates[0].text, "你好");
        assert_eq!(before.selected, 0);
    }

    /// 退格清空 raw → Cancel，会话结束。
    #[test]
    fn backspace_to_empty_cancels() {
        let engine = engine_with(Dict::from_entries(vec![("ni".into(), "你".into(), 1)]));
        let effects = run_script(
            &engine,
            &[
                Key::Char('n'),
                Key::Char('i'),
                Key::Backspace,
                Key::Backspace,
            ],
        );
        assert_eq!(effects.last().unwrap().end, Some(SessionEnd::Cancel));
    }

    /// 批处理翻页收集全表候选：8 条候选跨 2 页（page_size=5）。
    #[test]
    fn batch_collects_all_pages() {
        let engine = engine_with(Dict::from_entries(vec![
            ("ni'hao".into(), "你好".into(), 8000),
            ("ni'hao".into(), "泥嚎".into(), 7000),
            ("ni'hao".into(), "拟好".into(), 6000),
            ("ni'hao".into(), "你号".into(), 5000),
            ("ni'hao".into(), "尼好".into(), 4000),
            ("ni'hao".into(), "腻好".into(), 3000),
            ("ni'hao".into(), "逆豪".into(), 2000),
            ("ni'hao".into(), "你浩".into(), 1000),
        ]));
        let e = collect_all(&engine, "nihao");
        assert_eq!(e.reading, "ni'hao");
        assert_eq!(e.candidates.len(), 8); // 整表跨页，证明 PageDown 生效
        assert_eq!(e.candidates[0].text, "你好");
    }

    #[test]
    fn parse_args_forms() {
        assert_eq!(parse_args(&[]), None);
        assert_eq!(
            parse_args(&["a.imedic".into()]),
            Some(("a.imedic".into(), None))
        );
        assert_eq!(
            parse_args(&["a".into(), "--batch".into(), "ni'hao".into()]),
            Some(("a".into(), Some("ni'hao".into())))
        );
        assert_eq!(parse_args(&["a".into(), "x".into()]), None);
        assert_eq!(
            parse_args(&["a".into(), "--batch".into(), "ni'hao".into(), "extra".into()]),
            None
        );
    }
}
