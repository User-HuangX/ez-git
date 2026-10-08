//! 仓库领域：打开 / 初始化仓库，以及 head、身份、状态干净与否这些仓库级基础能力。

use std::path::Path;

use anyhow::{Context, Result};
use git2::{Commit, ErrorCode, Repository};

/// 打开已存在的仓库。传入非仓库目录会报错，这是目录选择后的第一道校验。
pub fn git_get_repo(path: impl AsRef<Path>) -> Result<Repository> {
    Ok(Repository::open(path)?)
}

/// 打开仓库；目录还不是 Git 仓库就地初始化。
///
/// 返回是否新建了仓库，界面据此提示用户刚发生了什么。
pub fn open_or_init(path: &Path) -> Result<(Repository, bool)> {
    match Repository::open(path) {
        Ok(repo) => Ok((repo, false)),
        Err(err) if err.code() == ErrorCode::NotFound => {
            let repo = Repository::init(path)
                .with_context(|| format!("初始化 {} 失败", path.display()))?;
            Ok((repo, true))
        }
        Err(err) => Err(err).with_context(|| format!("打开 {} 失败", path.display())),
    }
}

/// 从 git 配置读提交身份，优先级和 `git config` 一致：仓库本地 → 全局 → 系统。
/// 没配的字段返回 `None`，由调用方决定怎么兜底。
pub fn git_identity(repo: &Repository) -> (Option<String>, Option<String>) {
    let Ok(config) = repo.config() else {
        return (None, None);
    };
    (
        config.get_string("user.name").ok(),
        config.get_string("user.email").ok(),
    )
}

pub(crate) fn head_commit(repo: &Repository) -> Result<Option<Commit<'_>>> {
    match repo.head() {
        Ok(head) => Ok(Some(head.peel_to_commit()?)),
        Err(err) if err.code() == ErrorCode::UnbornBranch => Ok(None),
        Err(err) => Err(err.into()),
    }
}
