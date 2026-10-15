//! 提交领域：暂存、取消暂存、提交、回退，以及界面用的高层「提交」动作。

use anyhow::{Result, bail};
use git2::{Commit, IndexAddOption, Oid, Repository, RepositoryState, Signature};

use crate::repo::{conflict_paths, git_get_repo, head_commit};

/// `git add -A`：把工作区的全部改动（含未跟踪文件、删除）加入暂存区。
pub fn git_all(repo: &Repository) -> Result<()> {
    git_stage(repo, &["*".to_string()])
}

/// 暂存的写入口：未解决的冲突路径不能进暂存区——那等于把带 `<<<<<<<` 的内容当成
/// 「已解决」提交。`git_all` 传的 `*` 也从这里过，所以只拦这一处。
fn guard_conflicts(repo: &Repository, paths: &[String]) -> Result<()> {
    let conflicts = conflict_paths(repo)?;
    let all = paths.iter().any(|path| path == "*");
    let hit = conflicts
        .iter()
        .find(|conflict| all || paths.iter().any(|path| path == *conflict));
    match hit {
        Some(conflict) => bail!(
            "{conflict} 还有未解决的冲突，请先在分支窗口里选「用我的 / 用他的」，或手动改完后标记已解决"
        ),
        None => Ok(()),
    }
}

/// `git add -- <paths>`：把这几个路径的当前内容写进暂存区（删除也照记）。
pub fn git_stage(repo: &Repository, paths: &[String]) -> Result<()> {
    guard_conflicts(repo, paths)?;
    let mut index = repo.index()?;
    index.add_all(paths.iter(), IndexAddOption::DEFAULT, None)?;
    index.write()?;
    Ok(())
}

/// 取消暂存：把这些路径在暂存区里还原成 HEAD 的样子；HEAD 里没有的新文件退回未跟踪。
pub fn git_unstage(repo: &Repository, paths: &[String]) -> Result<()> {
    let head = head_commit(repo)?.map(|commit| commit.id());
    let target = match head {
        Some(oid) => Some(repo.find_object(oid, None)?),
        None => None,
    };
    repo.reset_default(target.as_ref(), paths.iter())?;
    Ok(())
}

/// `git commit -m <message>`：把**当前暂存区**提交掉，返回新提交的 id。
///
/// 不含 `git add`，需要先调 [`git_stage`] / [`git_all`]。
/// 暂存区与 HEAD 内容一致时报错，避免产生空提交；提交身份由调用方给定
/// （比如读 config.toml 里的 author/email）。
///
/// 如果正处于合并 / cherry-pick 的中间状态，这次提交就是收尾，函数会顺带清理该状态。
pub fn git_commit(repo: &Repository, message: &str, author: &Signature) -> Result<Oid> {
    match repo.state() {
        RepositoryState::Clean
        | RepositoryState::Merge
        | RepositoryState::CherryPick
        | RepositoryState::Revert => {}
        state => bail!("不能用普通 commit 完成 {state:?}"),
    }
    let mut index = repo.index()?;
    if index.has_conflicts() {
        bail!("暂存区仍有冲突，请先解决冲突并暂存");
    }
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let mut parents: Vec<Commit> = head_commit(repo)?.into_iter().collect();
    if repo.state() == RepositoryState::Merge {
        for id in std::fs::read_to_string(repo.path().join("MERGE_HEAD"))?.lines() {
            parents.push(repo.find_commit(Oid::from_str(id)?)?);
        }
    }
    if parents.len() < 2
        && parents
            .first()
            .is_some_and(|head| head.tree_id() == tree_id)
    {
        bail!("没有需要提交的改动");
    }
    let parent_refs: Vec<&Commit> = parents.iter().collect();
    let oid = repo.commit(Some("HEAD"), author, author, message, &tree, &parent_refs)?;
    if repo.state() != RepositoryState::Clean {
        repo.cleanup_state()?;
    }
    Ok(oid)
}

/// 提交前的公共部分：校验说明、打开仓库、准备提交身份。
fn prepare(message: &str) -> Result<(Repository, Signature<'static>)> {
    if message.trim().is_empty() {
        bail!("提交说明不能为空");
    }

    let path = crate::config::current_repo()?;
    let repo = git_get_repo(&path)?;
    let identity = crate::config::RepoConfig::resolved(&path)?;
    if identity.author.trim().is_empty() || identity.email.trim().is_empty() {
        bail!("先配置作者和邮箱（配置窗口，或 git config user.name / user.email）");
    }

    let signature = git2::Signature::now(&identity.author, &identity.email)?;
    Ok((repo, signature))
}

/// 暂存工作区全部改动后提交，返回新提交的 id。
pub fn commit_all(message: &str) -> Result<Oid> {
    let (repo, signature) = prepare(message)?;
    git_all(&repo)?;
    git_commit(&repo, message.trim(), &signature)
}

/// 只提交暂存区里的内容：提交模式下打钩过的文件，正好就是暂存区的内容。
pub fn commit_staged(message: &str) -> Result<Oid> {
    let (repo, signature) = prepare(message)?;
    git_commit(&repo, message.trim(), &signature)
}
