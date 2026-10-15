use std::fs;
use std::path::{Path, PathBuf};

use super::{clone_target, git_clone, git_push};

/// 每个测试自己的临时目录。
fn temp_dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ez-git-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

/// 造一个有提交的仓库，当作本地「远程」。
fn source_repo(root: &Path) -> anyhow::Result<PathBuf> {
    let source = root.join("source");
    let repo = git2::Repository::init(&source)?;
    fs::write(source.join("a.txt"), "hello\n")?;
    crate::commit::git_all(&repo)?;
    crate::commit::git_commit(
        &repo,
        "init",
        &git2::Signature::now("Test", "test@example.com")?,
    )?;
    Ok(source)
}

/// 推送链路：本地仓库设好 origin → git_push → 裸仓库里出现同一个分支。
/// 远程用本地裸仓库，所以这条检查不碰网络也不碰凭据。
#[test]
fn push_reaches_a_local_bare_remote() -> anyhow::Result<()> {
    let root = temp_dir("push");
    let bare = root.join("bare");
    let source = source_repo(&root)?;
    git2::Repository::init_bare(&bare)?;

    let repo = git2::Repository::open(&source)?;
    repo.remote("origin", bare.to_str().unwrap())?;
    git_push(&repo)?;

    let pushed = git2::Repository::open_bare(&bare)?;
    assert!(
        pushed.find_reference("refs/heads/master").is_ok()
            || pushed.find_reference("refs/heads/main").is_ok(),
        "裸仓库里没有推上去的分支"
    );

    fs::remove_dir_all(&root)?;
    Ok(())
}

/// 克隆目标：不存在就连父目录一起建，非空目录里让出 `<仓库名>` 子目录。
#[test]
fn clone_target_makes_room_for_the_repo() {
    let root = temp_dir("clone-target");

    // 还不存在的多层路径：直接建
    let fresh = root.join("a/b/new");
    assert_eq!(
        clone_target(&fresh, "git@host:owner/repo.git").unwrap(),
        fresh
    );

    // 空目录：就地克隆
    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        clone_target(&empty, "https://host/owner/repo.git").unwrap(),
        empty
    );

    // 非空目录：让出仓库名子目录
    fs::write(empty.join("keep.txt"), "x").unwrap();
    assert_eq!(
        clone_target(&empty, "https://host/owner/repo.git").unwrap(),
        empty.join("repo")
    );

    fs::remove_dir_all(&root).unwrap();
}

/// 真克隆一次：远程是本地仓库，目标目录里已经有别的文件。
#[test]
fn clone_lands_next_to_existing_files() -> anyhow::Result<()> {
    let root = temp_dir("clone");
    let source = source_repo(&root)?;

    let picked = root.join("picked");
    fs::create_dir_all(&picked)?;
    fs::write(picked.join("别的东西.txt"), "keep\n")?;

    let dir = git_clone(source.to_str().unwrap(), &picked)?;
    assert_eq!(dir, picked.join("source"));
    assert!(dir.join("a.txt").exists(), "克隆下来的文件不在");
    assert!(picked.join("别的东西.txt").exists(), "原有文件被动了");

    fs::remove_dir_all(&root)?;
    Ok(())
}
