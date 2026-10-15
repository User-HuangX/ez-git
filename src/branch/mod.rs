//! 分支管理：libgit2 读取状态，系统 Git 执行操作并保留标准的恢复流程。

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, TryLockError};

use anyhow::{Context, Result, bail};
use git2::{BranchType, ErrorCode, Oid, RepositoryState, StatusOptions};

use crate::config::RepoConfig;
use crate::repo::{conflict_paths, git_get_repo};

pub(crate) mod auto_pull;
mod history;
pub(crate) mod window;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Progress {
    #[default]
    Idle,
    Merge,
    Rebase,
    Other,
}

impl Progress {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Idle => "正常",
            Self::Merge => "合并中",
            Self::Rebase => "变基中",
            Self::Other => "其他 Git 操作进行中",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BranchInfo {
    pub name: String,
    pub reference: String,
    pub remote: bool,
    pub oid: Oid,
    pub summary: String,
    pub upstream: Option<String>,
    pub tracking: Option<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub branches: Vec<BranchInfo>,
    pub history: Vec<history::CommitNode>,
    pub current: Option<String>,
    pub head: Option<Oid>,
    pub remotes: Vec<String>,
    pub dirty: usize,
    pub progress: Progress,
    pub conflicts: Vec<String>,
}

/// 工作树、分支和冲突状态来自同一个仓库；窗口打开后不跟随主窗口换仓库。
pub(crate) fn snapshot(path: &Path) -> Result<Snapshot> {
    let repo = git_get_repo(path)?;
    if repo.is_bare() {
        bail!("裸仓库没有工作目录，不能在此切换或合并分支");
    }
    let head = match repo.head() {
        Ok(head) => Some(head.peel_to_commit()?.id()),
        Err(err) if matches!(err.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => None,
        Err(err) => return Err(err.into()),
    };
    let current = repo
        .find_reference("HEAD")?
        .symbolic_target()?
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_string);
    let progress = match repo.state() {
        RepositoryState::Clean => Progress::Idle,
        RepositoryState::Merge => Progress::Merge,
        RepositoryState::Rebase
        | RepositoryState::RebaseInteractive
        | RepositoryState::RebaseMerge => Progress::Rebase,
        _ => Progress::Other,
    };
    let mut branches = Vec::new();
    for branch in repo.branches(None)? {
        let (branch, kind) = branch?;
        // origin/HEAD 是别名，不是一个独立的操作目标。
        if branch.get().symbolic_target()?.is_some() {
            continue;
        }
        let name = branch.name()?.context("分支名称不是 UTF-8")?.to_string();
        let reference = branch
            .get()
            .name()
            .context("分支引用不是 UTF-8")?
            .to_string();
        let commit = branch.get().peel_to_commit()?;
        let upstream = if kind == BranchType::Local {
            match branch.upstream() {
                Ok(upstream) => Some(upstream),
                Err(err) if err.code() == ErrorCode::NotFound => None,
                Err(err) => return Err(err.into()),
            }
        } else {
            None
        };
        let tracking = upstream
            .as_ref()
            .and_then(|upstream| upstream.get().target())
            .map(|oid| repo.graph_ahead_behind(commit.id(), oid))
            .transpose()?;
        branches.push(BranchInfo {
            name,
            reference,
            remote: kind == BranchType::Remote,
            oid: commit.id(),
            summary: commit.summary()?.unwrap_or("（无提交标题）").to_string(),
            upstream: upstream
                .as_ref()
                .map(|branch| branch.name())
                .transpose()?
                .flatten()
                .map(str::to_string),
            tracking,
        });
    }
    branches.sort_by(|a, b| {
        (
            a.remote,
            current.as_deref() != Some(a.name.as_str()),
            &a.name,
        )
            .cmp(&(
                b.remote,
                current.as_deref() != Some(b.name.as_str()),
                &b.name,
            ))
    });
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let dirty = repo.statuses(Some(&mut opts))?.len();
    let conflicts = conflict_paths(&repo)?;
    let history = history::load(&repo, &branches, head)?;
    Ok(Snapshot {
        history,
        branches,
        current,
        head,
        remotes: repo
            .remotes()?
            .iter()
            .map(|name| Ok(name?.context("远程名称不是 UTF-8")?.to_string()))
            .collect::<Result<Vec<_>>>()?,
        dirty,
        progress,
        conflicts: conflicts.into_iter().collect(),
    })
}

/// 冲突文件取哪一侧的内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// 取「我们」的版本（git checkout --ours）：Merge 时是当前分支，Rebase 时是目标分支。
    Ours,
    /// 取「他们」的版本（git checkout --theirs）：Merge 时是合入分支，Rebase 时是被重放的提交。
    Theirs,
    /// 用户已经在编辑器里改好，按工作区内容暂存。
    Manual,
}

#[derive(Clone, Debug)]
pub(crate) enum Operation {
    Refresh,
    Create(String),
    Fetch(String),
    Pull,
    Switch(String),
    Merge(String),
    Rebase(String),
    SetUpstream(String),
    /// 解决一个冲突文件：取某一侧的内容，或者用户改完后按工作区内容暂存。
    Resolve {
        file: String,
        side: Choice,
    },
    Continue,
    Abort,
    Skip,
}

/// 无 shell 拼接；Git 自己处理 SSH 配置、凭据、reflog 和 merge/rebase 恢复状态。
fn git(path: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(path)
        .args(["-c", "color.ui=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .stdin(Stdio::null());
    // 只有可能写提交的操作需要身份；身份文件损坏也不能挡住 abort。
    if args
        .first()
        .is_some_and(|command| matches!(*command, "merge" | "rebase"))
        && !args.contains(&"--abort")
    {
        let identity = RepoConfig::resolved(path)?;
        if !identity.author.trim().is_empty() {
            command.args(["-c", &format!("user.name={}", identity.author)]);
        }
        if !identity.email.trim().is_empty() {
            command.args(["-c", &format!("user.email={}", identity.email)]);
        }
    }
    // 默认 SSH 不向终端等待口令；显式配置的 SSH 命令仍由用户自己的配置决定。
    if std::env::var_os("GIT_SSH_COMMAND").is_none()
        && std::env::var_os("GIT_SSH").is_none()
        && git_get_repo(path)?
            .config()?
            .get_string("core.sshCommand")
            .is_err()
    {
        command.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
    }
    let output = command
        .args(args)
        .output()
        .context("运行 Git 失败，请先安装系统 Git")?;
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .trim()
    .to_string();
    if !output.status.success() {
        bail!("git {} 失败：{}", args.first().unwrap_or(&""), message);
    }
    Ok(if message.is_empty() {
        "操作完成".to_string()
    } else {
        message
    })
}

fn require_idle(state: &Snapshot) -> Result<()> {
    if state.progress != Progress::Idle {
        bail!("{}，请先继续或中止当前操作", state.progress.label());
    }
    Ok(())
}

fn require_clean(state: &Snapshot) -> Result<()> {
    require_idle(state)?;
    if state.dirty != 0 {
        bail!(
            "有 {} 个未提交的改动（含暂存/未跟踪文件），请先提交后再操作",
            state.dirty
        );
    }
    Ok(())
}

fn require_head(state: &Snapshot) -> Result<()> {
    if state.head.is_none() {
        bail!("仓库还没有提交，请先完成第一次提交");
    }
    Ok(())
}

fn branch_target<'a>(state: &'a Snapshot, reference: &str) -> Result<&'a BranchInfo> {
    state
        .branches
        .iter()
        .find(|branch| branch.reference == reference)
        .context("目标分支不存在，请刷新列表")
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutoPullResult {
    Pulled(String),
    Skipped(&'static str),
}

/// 自动任务不排队：用户在操作时跳过，本次周期不会追赶重试。
pub(crate) fn try_auto_pull(path: &Path) -> Result<AutoPullResult> {
    let _guard = match GIT_OPERATIONS.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => {
            return Ok(AutoPullResult::Skipped("另一个分支操作正在执行"));
        }
        Err(TryLockError::Poisoned(_)) => bail!("Git 操作锁已损坏"),
    };
    auto_pull_locked(path)
}

fn auto_pull_locked(path: &Path) -> Result<AutoPullResult> {
    let state = snapshot(path)?;
    let skip = if state.progress != Progress::Idle {
        Some("合并或变基等 Git 操作正在进行")
    } else if state.dirty != 0 {
        Some("有未提交改动（含暂存和未跟踪文件）")
    } else if state.head.is_none() {
        Some("仓库还没有提交")
    } else if state.current.is_none() {
        Some("当前是 detached HEAD")
    } else if !state.branches.iter().any(|branch| {
        !branch.remote && Some(&branch.name) == state.current.as_ref() && branch.upstream.is_some()
    }) {
        Some("当前分支没有配置上游")
    } else {
        None
    };
    if let Some(reason) = skip {
        return Ok(AutoPullResult::Skipped(reason));
    }
    execute_locked(path, &Operation::Pull, &state).map(AutoPullResult::Pulled)
}

// ponytail: 跨仓库共享一把锁；需要并行操作多个仓库时换成仓库级锁。
static GIT_OPERATIONS: Mutex<()> = Mutex::new(());

/// 修改前再次读状态；窗口内容过期时拒绝执行，不操作用户刚切到的另一条分支。
pub(crate) fn execute(path: &Path, operation: &Operation, expected: &Snapshot) -> Result<String> {
    if matches!(operation, Operation::Refresh) {
        return execute_locked(path, operation, expected);
    }
    let _guard = GIT_OPERATIONS
        .lock()
        .map_err(|_| anyhow::anyhow!("Git 操作锁已损坏"))?;
    execute_locked(path, operation, expected)
}

fn execute_locked(path: &Path, operation: &Operation, expected: &Snapshot) -> Result<String> {
    let state = snapshot(path)?;
    if !matches!(operation, Operation::Refresh | Operation::Fetch(_))
        && (state.head != expected.head
            || state.current != expected.current
            || state.progress != expected.progress)
    {
        bail!("仓库 HEAD 或操作状态已变更，请刷新分支窗口后重试");
    }
    match operation {
        Operation::Refresh => Ok("分支列表已刷新".to_string()),
        Operation::Create(name) => {
            require_idle(&state)?;
            require_head(&state)?;
            if name.starts_with('-')
                || !git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
            {
                bail!("分支名称不合法：{name}");
            }
            git(path, &["branch", "--", name])
        }
        Operation::Fetch(remote) => {
            if !state.remotes.contains(remote) {
                bail!("远程 {remote} 不存在，请先配置远程地址");
            }
            git(path, &["fetch", "--prune", "--", remote])
        }
        Operation::Pull => {
            require_clean(&state)?;
            require_head(&state)?;
            let current = state
                .current
                .as_ref()
                .context("detached HEAD 不能 Pull，请先切换到本地分支")?;
            if !state.branches.iter().any(|branch| {
                &branch.name == current && !branch.remote && branch.upstream.is_some()
            }) {
                bail!("当前分支没有上游，请选择远程分支后点击「设为上游」");
            }
            // 不暗中 merge/rebase，也不自动 stash；分叉由用户选择如何整合。
            git(
                path,
                &["pull", "--ff-only", "--no-rebase", "--no-autostash"],
            )
            .context("Pull 只允许快进；若已分叉，请 Fetch 后选择上游执行 Merge 或 Rebase")
        }
        Operation::Switch(reference) => {
            require_clean(&state)?;
            let target = branch_target(&state, reference)?;
            if target.remote {
                bail!("只能 Switch 到本地分支");
            }
            git(path, &["switch", "--no-guess", "--", &target.name])
        }
        Operation::Merge(reference) | Operation::Rebase(reference) => {
            require_clean(&state)?;
            require_head(&state)?;
            let current = state
                .current
                .as_ref()
                .context("detached HEAD 不能 Merge/Rebase，请先切到本地分支")?;
            branch_target(&state, reference)?;
            if reference == &format!("refs/heads/{current}") {
                bail!("目标不能是当前分支");
            }
            match operation {
                Operation::Merge(_) => git(
                    path,
                    &["merge", "--no-edit", "--no-autostash", "--", reference],
                ),
                _ => git(path, &["rebase", "--no-autostash", reference]),
            }
        }
        Operation::SetUpstream(reference) => {
            require_idle(&state)?;
            let current = state
                .current
                .as_ref()
                .context("detached HEAD 没有可设置的上游")?;
            let target = branch_target(&state, reference)?;
            if !target.remote {
                bail!("请选择远程跟踪分支作为上游");
            }
            git(
                path,
                &[
                    "branch",
                    &format!("--set-upstream-to={reference}"),
                    "--",
                    current,
                ],
            )
        }
        Operation::Resolve { file, side } => {
            if !matches!(state.progress, Progress::Merge | Progress::Rebase)
                || !state.conflicts.contains(file)
            {
                bail!("该文件不是当前合并/变基的冲突文件，请刷新后重试");
            }
            match side {
                // 用户确认已在编辑器解决，再暂存该路径；删除冲突也能标记为解决。
                Choice::Manual => git(path, &["add", "-A", "--", file]),
                Choice::Ours | Choice::Theirs => {
                    let flag = if matches!(side, Choice::Theirs) {
                        "--theirs"
                    } else {
                        "--ours"
                    };
                    // 这一侧在索引里没有条目时（修改/删除冲突）checkout 会失败，
                    // 「取这一侧」就是删除文件；真出错时 git rm 的报错更直接。
                    if git(path, &["checkout", flag, "--", file]).is_err() {
                        return git(path, &["rm", "-f", "--", file]);
                    }
                    git(path, &["add", "-A", "--", file])
                }
            }
        }
        Operation::Continue => {
            if !state.conflicts.is_empty() {
                bail!(
                    "还有 {} 个冲突文件，请解决并标记后再继续",
                    state.conflicts.len()
                );
            }
            match state.progress {
                Progress::Merge => git(path, &["merge", "--continue"]),
                Progress::Rebase => git(path, &["rebase", "--continue"]),
                _ => bail!("没有可继续的合并/变基"),
            }
        }
        Operation::Skip => {
            if state.progress != Progress::Rebase {
                bail!("只有变基中可以跳过当前提交");
            }
            git(path, &["rebase", "--skip"])
        }
        Operation::Abort => match state.progress {
            Progress::Merge => git(path, &["merge", "--abort"]),
            Progress::Rebase => git(path, &["rebase", "--abort"]),
            _ => bail!("没有可中止的合并/变基"),
        },
    }
}
