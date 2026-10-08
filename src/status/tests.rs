use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{DiffKind, LineStats, Node, git_file_diff, git_line_stats, tree};

#[test]
fn line_stats_match_diff_without_double_counting_staged_changes() -> anyhow::Result<()> {
    let path = std::env::temp_dir().join(format!(
        "ez-git-line-stats-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let repo = git2::Repository::init(&path)?;

    // 没有 HEAD、末行没有换行：两行都算新增，不把 EOF 标记当作一行。
    fs::write(path.join("untracked.txt"), "first\nlast")?;
    let stats = git_line_stats(&repo)?["untracked.txt"];
    assert_eq!((stats.added, stats.removed), (2, 0));

    fs::create_dir(path.join("src"))?;
    fs::write(path.join("src/edit.txt"), "keep\nremove\n")?;
    fs::write(path.join("src/deleted.txt"), "gone\n")?;
    fs::write(path.join("src/clean.txt"), "unchanged\n")?;
    fs::write(path.join("src/binary.dat"), b"binary\0old")?;
    fs::write(path.join(".gitignore"), "ignored.txt\n")?;
    crate::commit::git_all(&repo)?;
    crate::commit::git_commit(
        &repo,
        "base",
        &git2::Signature::now("Test", "test@example.com")?,
    )?;
    assert!(git_line_stats(&repo)?.is_empty());

    fs::write(path.join("src/edit.txt"), "keep\nstaged\n")?;
    fs::remove_file(path.join("src/deleted.txt"))?;
    let mut index = repo.index()?;
    index.add_path(Path::new("src/edit.txt"))?;
    index.remove_path(Path::new("src/deleted.txt"))?;
    index.write()?;
    drop(index);
    fs::write(path.join("src/edit.txt"), "keep\nworktree\nextra\n")?;
    fs::write(path.join("new.txt"), "first\nlast")?;
    fs::write(path.join("src/binary.dat"), b"binary\0new")?;
    fs::write(path.join("ignored.txt"), "not counted\n")?;

    let counts = git_line_stats(&repo)?
        .into_iter()
        .map(|(path, stats)| (path, (stats.added, stats.removed)))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        counts,
        BTreeMap::from([
            ("new.txt".into(), (2, 0)),
            ("src/binary.dat".into(), (0, 0)),
            ("src/deleted.txt".into(), (0, 1)),
            ("src/edit.txt".into(), (2, 1)),
        ])
    );
    let nodes = tree(&repo)?;
    let total = nodes.iter().map(Node::line_stats).sum::<LineStats>();
    assert_eq!((total.added, total.removed), (4, 2));
    let folder = nodes[0].line_stats();
    assert_eq!((folder.added, folder.removed), (2, 2));
    let diff = git_file_diff(&repo, "src/edit.txt")?;
    assert_eq!(
        (
            diff.iter()
                .filter(|line| line.kind == DiffKind::Added)
                .count(),
            diff.iter()
                .filter(|line| line.kind == DiffKind::Deleted)
                .count()
        ),
        (2, 1)
    );

    drop(repo);
    fs::remove_dir_all(path)?;
    Ok(())
}
