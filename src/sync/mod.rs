//! 远程同步领域：推送、上游分支解析及远程认证回调。

use anyhow::{Result, anyhow};

use git2::{Cred, CredentialType, PushOptions, RemoteCallbacks, Repository};

use crate::config::current_repo;
use crate::repo::git_get_repo;

pub(crate) mod ui;

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
    let mut attempts = 0;
    callbacks.credentials(move |url, username, allowed| {
        attempts += 1;
        // ponytail: 自动认证最多三次；多身份认证需要显式选择凭据。
        if attempts > 3 {
            return Err(git2::Error::from_str(
                "远程认证失败，请检查 ssh-agent 或 credential helper",
            ));
        }
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::SSH_KEY)
            && let Some(username) = username
        {
            return Cred::ssh_key_from_agent(username);
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            let config = git2::Config::open_default()?;
            return Cred::credential_helper(&config, url, username);
        }
        Cred::default()
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
