//! 配置领域：打开历史（LRU）、当前仓库，以及每个仓库自己的作者、邮箱。
//!
//! 仓库级配置存在 `<git dir>/ez-git.toml`：跟着仓库走，位于 `.git` 内也不会被提交。
//! 不修改 `~/.gitconfig`，推送认证留给 ssh-agent / credential helper。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub(crate) mod ui;
mod window;

/// 应用配置文件（相对运行目录）。
const CONFIG_FILE: &str = "./config.toml";
/// 仓库级配置文件名，放在 `<git dir>` 里。
const REPO_CONFIG_FILE: &str = "ez-git.toml";

/// 打开历史的上限；配置里写 0 表示用它。
const DEFAULT_HISTORY_LIMIT: usize = 20;

/// 应用级配置：打开过的仓库目录（最近使用在前），以及作者、邮箱兜底。
#[derive(Default, Deserialize, Serialize)]
pub struct Config {
    /// 打开历史的上限，可在 config.toml 里修改。
    #[serde(default)]
    pub history_limit: usize,
    #[serde(default)]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub email: String,
}

impl Config {
    /// 读应用配置；还没写过就返回默认值，首次运行不用先建文件。
    pub fn get_config() -> Result<Config> {
        match std::fs::read_to_string(CONFIG_FILE) {
            Ok(s) => {
                let config: Config =
                    toml::from_str(&s).with_context(|| format!("解析 {CONFIG_FILE} 失败"))?;
                Ok(config)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("读取 {CONFIG_FILE} 失败")),
        }
    }

    /// 打开历史的上限。
    pub fn history_limit(&self) -> usize {
        match self.history_limit {
            0 => DEFAULT_HISTORY_LIMIT,
            limit => limit,
        }
    }

    /// 打开历史，最近使用的排在最前。
    pub fn history(&self) -> Vec<PathBuf> {
        self.repositories.iter().map(PathBuf::from).collect()
    }

    /// 当前仓库，取历史里最近打开的一个。
    pub fn current_repository(&self) -> Option<PathBuf> {
        self.repositories.first().map(PathBuf::from)
    }

    /// 记录一次打开：去重后放到最前，超出上限的从尾部淘汰，然后写回配置文件。
    pub fn record_open(repo: &Path) -> Result<()> {
        let mut config = Config::get_config()?;
        let repo = repo.to_string_lossy().into_owned();
        config.repositories.retain(|existing| existing != &repo);
        config.repositories.insert(0, repo);
        let limit = config.history_limit();
        config.repositories.truncate(limit);
        config.save()
    }

    /// 写回应用配置文件。
    pub fn save(&self) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        std::fs::write(CONFIG_FILE, text).with_context(|| format!("写入 {CONFIG_FILE} 失败"))
    }
}

/// 当前仓库路径；还没选就报错说明，界面直接显示这句话。
pub fn current_repo() -> Result<PathBuf> {
    Config::get_config()?
        .current_repository()
        .ok_or_else(|| anyhow::anyhow!("先在配置里选择仓库"))
}

/// 单个仓库的设置。
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RepoConfig {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub email: String,
}

impl RepoConfig {
    /// 载入仓库身份：`.git/ez-git.toml` 里空着的字段按 git 配置、环境变量、应用配置兜底，
    /// 所以用户不用每次重填机器上早就配好的作者和邮箱。
    pub fn resolved(repo: &Path) -> Result<Self> {
        let mut config = Self::load(repo)?;
        let (author, email) = default_identity(repo);
        if config.author.trim().is_empty() {
            config.author = author;
        }
        if config.email.trim().is_empty() {
            config.email = email;
        }
        Ok(config)
    }

    /// 读仓库配置；还没配置过就返回默认值。
    pub fn load(repo: &Path) -> Result<Self> {
        let path = config_path(repo)?;
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("解析 {} 失败", path.display()))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("读取 {} 失败", path.display())),
        }
    }

    pub fn save(&self, repo: &Path) -> Result<()> {
        let path = config_path(repo)?;
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("写入 {} 失败", path.display()))
    }
}

/// 仓库配置文件路径。非 Git 目录直接报错，所以选择目录时只接受已初始化的仓库。
/// linked worktree 使用公共 git 目录，让配置在各工作树间共享。
pub fn config_path(repo: &Path) -> Result<PathBuf> {
    let repository = crate::repo::git_get_repo(repo)
        .with_context(|| format!("{} 不是 Git 仓库", repo.display()))?;
    Ok(repository.commondir().join(REPO_CONFIG_FILE))
}

/// 默认提交身份：git 配置 → 环境变量 → 应用 config.toml。
fn default_identity(repo: &Path) -> (String, String) {
    let (git_author, git_email) = crate::repo::git_get_repo(repo)
        .map(|repository| crate::repo::git_identity(&repository))
        .unwrap_or_default();
    let app = Config::get_config().unwrap_or_default();

    (
        git_author
            .or_else(|| env_var("GIT_AUTHOR_NAME"))
            .unwrap_or(app.author),
        git_email
            .or_else(|| env_var("GIT_AUTHOR_EMAIL"))
            .or_else(|| env_var("EMAIL"))
            .unwrap_or(app.email),
    )
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// 提交身份的必要字段：作者、邮箱都不能为空。
pub fn validate(author: &str, email: &str) -> Result<()> {
    if author.trim().is_empty() {
        bail!("作者不能为空");
    }
    if email.trim().is_empty() {
        bail!("邮箱不能为空");
    }
    if !email.contains('@') {
        bail!("邮箱格式不正确：{email}");
    }
    Ok(())
}
