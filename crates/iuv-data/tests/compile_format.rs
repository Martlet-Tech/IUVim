//! iuv-data 集成测试：rime yaml 编译 → 二进制 → `load` 全链路。
//! 任务书 10 §4 用例；fixture 一律落 `std::env::temp_dir()`，不写 repo 目录。

use std::path::{Path, PathBuf};

use iuv_data::{compile_files, load, CompileStats, Entry};

/// 每个测试独立临时目录，避免并行冲突。
fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iuv_data_it_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_yaml(dir: &Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

/// 编译单文件 yaml，返回输出路径与统计。
fn compile_one(dir: &Path, yaml: &str) -> (PathBuf, CompileStats) {
    let input = write_yaml(dir, "in.dict.yaml", yaml);
    let output = dir.join("out.imedic");
    let stats = compile_files(&[input], &output).unwrap();
    (output, stats)
}

#[test]
fn roundtrip_small_dict() {
    let dir = tmp_dir("roundtrip");
    let (out, stats) = compile_one(
        &dir,
        "你好\tni hao\t1000\n泥嚎\tni hao\t100\n你好\tni hao\t9999\n好的\thao de\n",
    );
    assert_eq!(stats.files, 1);
    assert_eq!(stats.entries, 6); // 3 全拼 + 3 简拼键（你好→nh、泥嚎→nh、好的→hd）
    assert_eq!(stats.codes, 4); // ni'hao、hao'de + 简拼 nh、hd
    assert_eq!(stats.duplicates, 1);

    let d = load(&out).unwrap();
    let nihao = d.exact("ni'hao");
    assert_eq!(nihao.len(), 2);
    assert_eq!(nihao[0].word, "你好");
    assert_eq!(nihao[0].weight, 9999); // 去重取最大 weight（与出现顺序无关）
    assert_eq!(nihao[1].weight, 100);
    assert_eq!(d.exact("hao'de")[0].weight, 0); // 权重缺省按 0
    // 简拼键：同键多条按 weight 降序，权重复制自原词条
    let nh = d.exact("nh");
    assert_eq!(nh.len(), 2);
    assert_eq!(nh[0].word, "你好");
    assert_eq!(nh[0].weight, 9999);
    assert_eq!(nh[1].word, "泥嚎");
}

#[test]
fn yaml_header_and_comments_skipped() {
    let dir = tmp_dir("header");
    let (out, stats) = compile_one(
        &dir,
        "\u{feff}# 顶部注释\n---\nname: test.dict\nversion: \"0.1\"\nsort: by_weight\n...\n# 词条前注释\n\n你好\tni hao\t1000\n\n好的\thao de\t200\n",
    );
    assert_eq!(stats.entries, 4); // 2 全拼 + 2 简拼键（你好→nh、好的→hd）
    assert_eq!(stats.duplicates, 0);
    let d = load(&out).unwrap();
    assert_eq!(d.exact("ni'hao")[0].weight, 1000);
}

#[test]
fn code_keeps_separation() {
    let dir = tmp_dir("squash");
    let (out, _) = compile_one(&dir, "你好\tNi Hao\t100\n");
    let d = load(&out).unwrap();
    let hits = d.exact("ni'hao");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].code, "ni'hao");
    assert!(d.exact("Ni Hao").is_empty());
    assert!(d.exact("nihao ").is_empty());
}

#[test]
fn bad_magic_rejected() {
    let dir = tmp_dir("bad_magic");
    let (out, _) = compile_one(&dir, "你好\tni hao\t100\n");
    let mut data = std::fs::read(&out).unwrap();
    data[0] = b'X';
    let corrupt = dir.join("corrupt.imedic");
    std::fs::write(&corrupt, &data).unwrap();
    let err = load(&corrupt).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("magic"));
}

#[test]
fn truncated_file_rejected() {
    let dir = tmp_dir("truncated");
    let (out, _) = compile_one(&dir, "你好\tni hao\t100\n");
    let mut data = std::fs::read(&out).unwrap();
    data.truncate(data.len() - 3); // 砍掉 weight 尾字节
    let trunc = dir.join("trunc.imedic");
    std::fs::write(&trunc, &data).unwrap();
    let err = load(&trunc).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn prefix_query_smoke() {
    let dir = tmp_dir("prefix");
    let (out, _) = compile_one(
        &dir,
        "你好\tni hao\t100\n你们\tni men\t50\n泥嚎\tni hao\t90\n拿手\tna shou\t10\n",
    );
    let d = load(&out).unwrap();
    // 词库键已分隔化（空格→'）：前缀联想用音节前缀 "ni" 命中 "ni'hao" / "ni'men"。
    let words: Vec<&str> = d
        .prefix("ni", 10)
        .iter()
        .map(|e| e.word.as_str())
        .collect();
    assert!(words.contains(&"你好"));
    assert!(words.contains(&"泥嚎"));
    assert!(words.contains(&"你们"));
    assert!(!words.contains(&"拿手"));
}

#[test]
fn syllables_collected() {
    let dir = tmp_dir("syllables");
    let (out, _) = compile_one(&dir, "你好\tni hao\t100\n好的\thao de\t50\n");
    let d = load(&out).unwrap();
    assert!(d.syllables().contains("ni"));
    assert!(d.syllables().contains("hao"));
    assert!(d.syllables().contains("de"));
}

#[test]
fn multi_file_merge_dedup() {
    let dir = tmp_dir("multi");
    let in1 = write_yaml(&dir, "a.dict.yaml", "你好\tni hao\t100\n");
    let in2 = write_yaml(
        &dir,
        "b.dict.yaml",
        "你好\tni hao\t200\n世界\tshi jie\t50\n",
    );
    let out = dir.join("merged.imedic");
    let stats = compile_files(&[in1, in2], &out).unwrap();
    assert_eq!(stats.files, 2);
    assert_eq!(stats.entries, 4); // 2 全拼 + 2 简拼键（你好→nh、世界→sj）
    assert_eq!(stats.codes, 4);
    assert_eq!(stats.duplicates, 1);
    let d = load(&out).unwrap();
    assert_eq!(d.exact("ni'hao")[0].weight, 200); // 跨文件去重取最大
    assert_eq!(d.exact("sj")[0].word, "世界");
}

#[test]
fn abbrev_key_generation() {
    let dir = tmp_dir("abbrev");
    let (out, _) = compile_one(
        &dir,
        "你好\tni hao\t100\n西安\txi an\t50\n天安门\ttian an men\t30\n是\tshi\t200\n",
    );
    let d = load(&out).unwrap();
    // 简拼键：每音节首字母，权重复制
    assert_eq!(d.exact("nh")[0].word, "你好");
    assert_eq!(d.exact("nh")[0].weight, 100);
    assert_eq!(d.exact("xa")[0].word, "西安"); // 强制分隔段也取首字母
    assert_eq!(d.exact("tam")[0].word, "天安门");
    // 单音节词不生成简拼键
    assert!(d.exact("s").is_empty());
    // 简拼键不污染音节表/词长统计
    assert!(!d.syllables().contains("nh"));
    assert_eq!(d.max_word_syllables(), 3);
}

#[test]
fn format_write_load_roundtrip() {
    let dir = tmp_dir("direct");
    let records = [
        Entry {
            word: "你好".into(),
            code: "ni'hao".into(),
            weight: 10,
        },
        Entry {
            word: "的".into(),
            code: "de".into(),
            weight: 5,
        },
    ];
    let mut buf = Vec::new();
    iuv_data::format::write(&records, &mut buf).unwrap();
    let p = dir.join("direct.imedic");
    std::fs::write(&p, &buf).unwrap();
    let d = load(&p).unwrap();
    assert_eq!(d.entry_count(), 2);
    assert_eq!(d.exact("ni'hao")[0].weight, 10);
}
