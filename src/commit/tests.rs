use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Status, StatusOptions};

use super::{git_all, git_commit, git_stage, git_unstage};

/// 一个有提交的临时仓库，外加一个改动过的 a.txt。
fn temp_repo(tag: &str) -> anyhow::Result<(PathBuf, Repository)> {
    let path = std::env::temp_dir().join(format!("ez-git-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path)?;
    let repo = Repository::init(&path)?;

    fs::write(path.join("a.txt"), "one\n")?;
    git_all(&repo)?;
    git_commit(
        &repo,
        "init",
        &git2::Signature::now("Test", "test@example.com")?,
    )?;
    fs::write(path.join("a.txt"), "two\n")?;
    Ok((path, repo))
}

/// 某个路径当前的状态位；没有状态就是 CURRENT。
fn status_of(repo: &Repository, path: &str) -> anyhow::Result<Status> {
    let mut opts = StatusOptions::new();
    opts.pathspec(Path::new(path)).include_untracked(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(statuses
        .iter()
        .next()
        .map(|entry| entry.status())
        .unwrap_or(Status::CURRENT))
}

/// 打钩 = 暂存，取消 = 退回：改过的文件、新文件、删掉的文件三种都要对。
#[test]
fn stage_and_unstage_follow_the_index() -> anyhow::Result<()> {
    let (path, repo) = temp_repo("stage")?;

    // 改过的文件：先只在工作区改动，暂存后进索引，取消后又只剩工作区改动
    assert_eq!(status_of(&repo, "a.txt")?, Status::WT_MODIFIED);
    git_stage(&repo, &["a.txt".to_string()])?;
    assert_eq!(status_of(&repo, "a.txt")?, Status::INDEX_MODIFIED);
    git_unstage(&repo, &["a.txt".to_string()])?;
    assert_eq!(status_of(&repo, "a.txt")?, Status::WT_MODIFIED);

    // 新文件（HEAD 里没有）：取消暂存要退回未跟踪，而不是删掉文件
    fs::write(path.join("new.txt"), "new\n")?;
    git_stage(&repo, &["new.txt".to_string()])?;
    assert_eq!(status_of(&repo, "new.txt")?, Status::INDEX_NEW);
    git_unstage(&repo, &["new.txt".to_string()])?;
    assert_eq!(status_of(&repo, "new.txt")?, Status::WT_NEW);
    assert!(path.join("new.txt").exists(), "取消暂存不该动工作区文件");

    // 删掉的文件：暂存要把删除记进索引（git add -A 的语义）
    fs::remove_file(path.join("a.txt"))?;
    git_stage(&repo, &["a.txt".to_string()])?;
    assert_eq!(status_of(&repo, "a.txt")?, Status::INDEX_DELETED);

    fs::remove_dir_all(&path)?;
    Ok(())
}
