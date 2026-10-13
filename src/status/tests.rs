use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use git2::Repository;

use super::diff::diff_text;
use super::{DiffKind, DiffLine, git_file_diff, git_line_stats, tree};

/// 临时基准：造一个大文件，量一下取 diff / 算行数 / 攒文本各要多久。
fn bench_repo(lines: usize) -> anyhow::Result<(PathBuf, Repository)> {
    let path = std::env::temp_dir().join(format!("ez-git-bench-{}", lines));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path)?;
    let repo = Repository::init(&path)?;

    let body: String = (0..lines)
        .map(|ix| format!("fn line_{ix}() -> usize {{ {ix} }}\n"))
        .collect();
    fs::write(path.join("big.rs"), &body)?;
    fs::write(path.join("small.txt"), "hello\n")?;
    crate::commit::git_all(&repo)?;
    crate::commit::git_commit(
        &repo,
        "init",
        &git2::Signature::now("Test", "test@example.com")?,
    )?;

    // 改三行，让 diff 有内容；再留一堆未跟踪文件
    let edited = body.replacen("fn line_0", "fn edited_0", 1);
    let edited = edited.replacen("fn line_100", "fn edited_100", 1);
    let edited = edited.replacen("fn line_9000", "fn edited_9000", 1);
    fs::write(path.join("big.rs"), edited)?;
    for ix in 0..50 {
        fs::write(path.join(format!("untracked_{ix}.txt")), "x\n".repeat(200))?;
    }
    Ok((path, repo))
}

#[test]
#[ignore = "基准，手动跑：cargo test --release bench -- --ignored --nocapture"]
fn bench_diff_paths() -> anyhow::Result<()> {
    let (path, repo) = bench_repo(20_000)?;

    let t = Instant::now();
    let stats = git_line_stats(&repo)?;
    println!(
        "git_line_stats: {:?}（{} 个文件）",
        t.elapsed(),
        stats.len()
    );

    let t = Instant::now();
    let lines = git_file_diff(&repo, "big.rs")?;
    let git_ms = t.elapsed();
    println!("git_file_diff(20k 行): {git_ms:?}（{} 行）", lines.len());

    let t = Instant::now();
    let text: String = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    println!("攒成整篇文本: {:?}（{} 字节）", t.elapsed(), text.len());

    let t = Instant::now();
    let nodes = tree(&repo)?;
    println!(
        "tree（整树重读）: {:?}（{} 个顶层节点）",
        t.elapsed(),
        nodes.len()
    );

    fs::remove_dir_all(&path)?;
    Ok(())
}

/// 连续同类行合成一个装饰：范围要对，中间被上下文隔开的两段不能并成一段。
#[test]
fn diff_text_merges_consecutive_runs() {
    let line = |kind, text: &str| DiffLine {
        kind,
        text: text.to_string(),
    };
    let lines = vec![
        line(DiffKind::Header, "@@ -1,2 +1,3 @@"),
        line(DiffKind::Context, " ctx"),
        line(DiffKind::Added, "+a"),
        line(DiffKind::Added, "+b"),
        line(DiffKind::Context, " ctx2"),
        line(DiffKind::Deleted, "-c"),
        line(DiffKind::Deleted, "-d"),
        line(DiffKind::Deleted, "-e"),
    ];

    let (text, marks) = diff_text(&lines);
    let spans: Vec<&str> = marks
        .iter()
        .map(|mark| &text[mark.range().clone()])
        .collect();
    assert_eq!(spans, vec!["+a\n+b", "-c\n-d\n-e"]);
}

/// 高亮器构造是不是固定开销的大头（每次换文本都会重建它）。
#[test]
#[ignore = "基准，手动跑：cargo test --release bench_highlighter -- --ignored --nocapture"]
fn bench_highlighter_init() {
    use gpui_kit::component::highlighter::SyntaxHighlighter;

    let t = Instant::now();
    let first = SyntaxHighlighter::new("rust");
    let first_time = t.elapsed();
    let t = Instant::now();
    let second = SyntaxHighlighter::new("rust");
    let second_time = t.elapsed();
    println!("SyntaxHighlighter::new: 第一次 {first_time:?}，第二次 {second_time:?}");
    drop((first, second));
}
