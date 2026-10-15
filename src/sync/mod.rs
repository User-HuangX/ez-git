//! 远程同步领域：推送、上游分支解析及远程认证回调。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use git2::build::RepoBuilder;
use git2::{Cred, CredentialType, FetchOptions, PushOptions, RemoteCallbacks, Repository};

use crate::config::current_repo;
use crate::repo::git_get_repo;

pub(crate) mod ui;

#[cfg(test)]
mod tests;

/// `git push`：把当前分支推到它的上游（没配上游时默认 origin/同名分支）。
pub fn git_push(repo: &Repository) -> Result<()> {
    let (local, remote_name, remote_branch) = current_upstream(repo)?;
    let refspec = format!("refs/heads/{local}:refs/heads/{remote_branch}");

    let mut remote = repo.find_remote(&remote_name)?;
    let mut opts = PushOptions::new();
    opts.remote_callbacks(remote_callbacks());
    remote.push(&[refspec.as_str()], Some(&mut opts))?;
    Ok(())
}
/// `git clone`：把远程克隆到 `dir`，返回真正落地的目录。
///
/// 目标不存在就连父目录一起建（可以直接填 `~/Project/新仓库`），已经存在的非空目录里
/// 让出 `<仓库名>` 子目录——libgit2 只肯克隆到不存在或者空的路径。
pub fn git_clone(url: &str, dir: &Path) -> Result<PathBuf> {
    let dir = clone_target(dir, url)?;
    let mut fetch = FetchOptions::new();
    fetch.remote_callbacks(remote_callbacks());
    RepoBuilder::new()
        .fetch_options(fetch)
        .clone(url, &dir)
        .with_context(|| format!("克隆 {url} 到 {} 失败", dir.display()))?;
    Ok(dir)
}

/// 克隆目标目录：不存在就建（含父目录），非空的让出 `<仓库名>` 子目录。
fn clone_target(dir: &Path, url: &str) -> Result<PathBuf> {
    // 已经存在且非空 → 在里面新建 `<仓库名>`
    let occupied = fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some());
    let dir = if occupied {
        dir.join(repo_name(url))
    } else {
        dir.to_path_buf()
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// URL 末段的仓库名：`git@host:owner/repo.git`、`https://host/owner/repo.git` → `repo`。
fn repo_name(url: &str) -> String {
    let tail = url.rsplit(['/', ':']).next().unwrap_or(url);
    match tail.strip_suffix(".git").unwrap_or(tail) {
        "" | "." => "repo".to_string(),
        name => name.to_string(),
    }
}

/// `~/.ssh` 里常见的私钥，按顺序挑第一个存在的。
fn private_key_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .into_iter()
        .map(|name| Path::new(&home).join(".ssh").join(name))
        .find(|path| path.exists())
}

/// 当前分支 → (本地分支名, 远程名, 远程上的分支名)。
///
/// 读的是 `branch.<name>.remote` / `branch.<name>.merge`；没配就按 origin + 同名分支处理，
/// 这样刚 clone 下来的仓库不用额外配置也能直接 push / pull。
fn current_upstream(repo: &Repository) -> Result<(String, String, String)> {
    let head = repo.head()?;
    let local = head
        .name()?
        .strip_prefix("refs/heads/")
        .ok_or_else(|| anyhow!("HEAD 不在分支上（detached），无法确定上游"))?
        .to_string();

    let config = repo.config()?;
    let remote = config
        .get_string(&format!("branch.{local}.remote"))
        .unwrap_or_else(|_| "origin".to_string());
    let merge_ref = config
        .get_string(&format!("branch.{local}.merge"))
        .unwrap_or_else(|_| format!("refs/heads/{local}"));
    let remote_branch = merge_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&merge_ref)
        .to_string();

    Ok((local, remote, remote_branch))
}

/// 网络操作需要的回调：ssh-agent + git credential helper 取凭据，
/// 并把服务器拒绝的 ref 变成错误（不然被拒绝的 push 也会「成功」返回）。
fn remote_callbacks() -> RemoteCallbacks<'static> {
    let mut callbacks = RemoteCallbacks::new();
    // 认证顺序：ssh-agent → ~/.ssh 里的私钥 → credential helper（https）。
    // 每种只试一次，试完还不行就报错，别在同一个凭据上打转。
    let (mut agent_tried, mut key_file_tried) = (false, false);
    callbacks.credentials(move |url, username, allowed| {
        let username = username.unwrap_or("git");
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username);
        }
        if allowed.contains(CredentialType::SSH_KEY) {
            if !agent_tried {
                agent_tried = true;
                return Cred::ssh_key_from_agent(username);
            }
            if !key_file_tried {
                key_file_tried = true;
                if let Some(path) = private_key_path() {
                    return Cred::ssh_key(username, None, &path, None);
                }
            }
            return Err(git2::Error::from_str(
                "远程认证失败：ssh-agent 和 ~/.ssh 里都没有能用的私钥",
            ));
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            let config = git2::Config::open_default()?;
            return Cred::credential_helper(&config, url, Some(username));
        }
        if allowed.contains(CredentialType::DEFAULT) {
            return Cred::default();
        }
        Err(git2::Error::from_str("远程认证失败：没有可用的凭据"))
    });
    callbacks.push_update_reference(|refname, status| match status {
        Some(message) => Err(git2::Error::from_str(&format!(
            "{refname} 被拒绝：{message}"
        ))),
        None => Ok(()),
    });
    callbacks
}

/// 把当前分支推给上游（界面上的「推送」按钮）。
pub fn push() -> Result<()> {
    let repo = git_get_repo(current_repo()?)?;
    git_push(&repo)
}
