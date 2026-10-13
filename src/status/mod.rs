//! 仓库状态领域：把工作区和暂存区的文件整理成一棵带改动标记的目录树。
//!
//! 树的节点来自「索引里的已跟踪文件」加上「状态里的未跟踪文件」，所以左侧能看到整个目录结构；
//! 改动类型决定界面上的颜色：新增绿、修改黄、删除红。

use std::collections::BTreeMap;

use anyhow::Result;
use git2::{Diff, DiffFormat, DiffOptions, Patch, Repository, StatusOptions};

use crate::repo::head_commit;

pub(crate) mod diff;
pub(crate) mod file_icons;

pub(crate) mod ui;

#[cfg(test)]
mod tests;

/// HEAD 到当前工作区的增删行数；二进制文件不计行数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineStats {
    pub added: usize,
    pub removed: usize,
}

impl std::iter::Sum for LineStats {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |total, stats| Self {
            added: total.added + stats.added,
            removed: total.removed + stats.removed,
        })
    }
}

/// 一次 diff 统计所有改动文件，暂存区与工作区不重复相加。
pub fn git_line_stats(repo: &Repository) -> Result<BTreeMap<String, LineStats>> {
    let diff = worktree_diff(repo, &mut DiffOptions::new())?;
    let mut files = BTreeMap::new();
    for (index, delta) in diff.deltas().enumerate() {
        let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) else {
            continue;
        };
        let stats = match Patch::from_diff(&diff, index)? {
            Some(patch) => {
                let (_, added, removed) = patch.line_stats()?;
                LineStats { added, removed }
            }
            None => LineStats::default(),
        };
        files.insert(path.to_string_lossy().into_owned(), stats);
    }
    Ok(files)
}

/// diff 里一行的类型，决定界面上的红绿标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    /// 文件头或 hunk 头
    Header,
    /// 没改动的原文行
    Context,
    /// 新增的行（绿）
    Added,
    /// 删除的行（红）
    Deleted,
}

/// diff 的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    /// 行内容，带 `+`/`-`/空格 前缀，和 `git diff` 的写法一致。
    pub text: String,
}

/// 单个文件从 HEAD 到工作区的改动，带**全量上下文**：
/// 所以等于把整个文件原文读出来，只是增行标绿、删行标红。
/// 未跟踪文件会整篇算新增，删除的文件整篇算删除。
pub fn git_file_diff(repo: &Repository, file: &str) -> Result<Vec<DiffLine>> {
    let mut opts = DiffOptions::new();
    opts.pathspec(file)
        .disable_pathspec_match(true)
        // 足够大就等于不省略任何上下文行
        .context_lines(FULL_CONTEXT);
    let diff = worktree_diff(repo, &mut opts)?;
    let lines = diff_lines(&diff)?;
    if lines.is_empty() {
        // 没有改动：直接把工作区原文按原文行返回，界面照样能读整篇文件
        return read_worktree_lines(repo, file);
    }
    Ok(lines)
}

fn worktree_diff<'repo>(repo: &'repo Repository, opts: &mut DiffOptions) -> Result<Diff<'repo>> {
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    let head = head_commit(repo)?.map(|commit| commit.tree()).transpose()?;
    Ok(repo.diff_tree_to_workdir_with_index(head.as_ref(), Some(opts))?)
}

/// 全量上下文的行数上限；比任何正常文件都大。
const FULL_CONTEXT: u32 = 1_000_000;

/// 没有改动时按上下文行读出工作区原文。
fn read_worktree_lines(repo: &Repository, file: &str) -> Result<Vec<DiffLine>> {
    let Some(path) = repo.workdir().map(|workdir| workdir.join(file)) else {
        return Ok(Vec::new());
    };
    // 读不出来（二进制等）就当没有可显示内容
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    Ok(text
        .lines()
        .map(|line| DiffLine {
            kind: DiffKind::Context,
            text: format!(" {line}"),
        })
        .collect())
}

fn diff_lines(diff: &Diff) -> Result<Vec<DiffLine>> {
    let mut lines = Vec::new();
    diff.print(DiffFormat::Patch, |_, _, line| {
        let kind = match line.origin() {
            '+' => DiffKind::Added,
            '-' => DiffKind::Deleted,
            'F' | 'H' => DiffKind::Header,
            _ => DiffKind::Context,
        };
        // print(Patch) 的 content 不带行首标记，自己补上 +/空格，和 `git diff` 长得一样
        let mut text = String::new();
        match kind {
            DiffKind::Added => text.push('+'),
            DiffKind::Deleted => text.push('-'),
            DiffKind::Context => text.push(' '),
            DiffKind::Header => {}
        }
        text.push_str(&String::from_utf8_lossy(line.content()));
        lines.push(DiffLine { kind, text });
        true
    })?;
    Ok(lines)
}

/// 文件相对 HEAD 和暂存区的改动类型，对应左侧树里的颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// 没有改动
    Clean,
    /// 新增：未跟踪，或者新进了暂存区（绿）
    Added,
    /// 修改：内容改动、改名、类型变化或冲突（黄）
    Modified,
    /// 删除：工作区或者暂存区里被删掉（红）
    Deleted,
}

/// 目录树节点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Dir {
        name: String,
        path: String,
        children: Vec<Node>,
        stats: LineStats,
    },
    File {
        name: String,
        path: String,
        change: Change,
        stats: LineStats,
        /// 暂存区里相对 HEAD 有改动；就是提交模式里的打钩状态。
        staged: bool,
    },
}

impl Node {
    /// 目录返回子节点的合计，文件返回自身的行数。
    pub fn line_stats(&self) -> LineStats {
        match self {
            Self::Dir { stats, .. } | Self::File { stats, .. } => *stats,
        }
    }
}

/// 读仓库里所有文件并标出改动类型；目录按名字排序并排在文件前面。
pub fn tree(repo: &Repository) -> Result<Vec<Node>> {
    // 索引里的文件就是已跟踪文件，默认没改动；状态再覆盖或补上未跟踪文件
    let mut files: BTreeMap<String, (Change, LineStats, bool)> = repo
        .index()?
        .iter()
        .map(|entry| {
            (
                String::from_utf8_lossy(&entry.path).into_owned(),
                (Change::Clean, LineStats::default(), false),
            )
        })
        .collect();

    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    for entry in repo.statuses(Some(&mut opts))?.iter() {
        files.insert(
            // 路径不保证是合法 UTF-8，保留 lossy 转换，避免整个状态读取失败。
            String::from_utf8_lossy(entry.path_bytes()).into_owned(),
            (
                change_of(entry.status()),
                LineStats::default(),
                is_staged(entry.status()),
            ),
        );
    }
    for (path, stats) in git_line_stats(repo)? {
        if let Some((_, counts, _)) = files.get_mut(&path) {
            *counts = stats;
        }
    }

    Ok(build(files))
}

/// 暂存区里相对 HEAD 有没有改动。
fn is_staged(status: git2::Status) -> bool {
    status.intersects(
        git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED
            | git2::Status::INDEX_DELETED
            | git2::Status::INDEX_RENAMED
            | git2::Status::INDEX_TYPECHANGE,
    )
}

/// 状态位 → 颜色。删除优先，其次修改，最后新增；冲突按修改处理。
fn change_of(status: git2::Status) -> Change {
    use git2::Status as S;

    if status.intersects(S::INDEX_DELETED | S::WT_DELETED) {
        Change::Deleted
    } else if status.intersects(
        S::INDEX_MODIFIED
            | S::INDEX_RENAMED
            | S::INDEX_TYPECHANGE
            | S::WT_MODIFIED
            | S::WT_TYPECHANGE
            | S::CONFLICTED,
    ) {
        Change::Modified
    } else if status.intersects(S::INDEX_NEW | S::WT_NEW) {
        Change::Added
    } else {
        Change::Clean
    }
}

/// 按路径里的 `/` 攒出嵌套结构。
#[derive(Default)]
struct Builder {
    dirs: BTreeMap<String, Builder>,
    files: BTreeMap<String, (Change, LineStats, bool)>,
}

impl Builder {
    fn insert(&mut self, path: &str, change: Change, stats: LineStats, staged: bool) {
        match path.split_once('/') {
            Some((dir, rest)) => self
                .dirs
                .entry(dir.to_string())
                .or_default()
                .insert(rest, change, stats, staged),
            None => {
                self.files.insert(path.to_string(), (change, stats, staged));
            }
        }
    }

    fn into_nodes(self, prefix: &str) -> Vec<Node> {
        let join = |name: &str| match prefix.is_empty() {
            true => name.to_string(),
            false => format!("{prefix}/{name}"),
        };

        self.dirs
            .into_iter()
            .map(|(name, builder)| {
                let path = join(&name);
                let children = builder.into_nodes(&path);
                let stats = children.iter().map(Node::line_stats).sum();
                Node::Dir {
                    children,
                    name,
                    path,
                    stats,
                }
            })
            .chain(
                self.files
                    .into_iter()
                    .map(|(name, (change, stats, staged))| Node::File {
                        path: join(&name),
                        name,
                        change,
                        stats,
                        staged,
                    }),
            )
            .collect()
    }
}

fn build(files: BTreeMap<String, (Change, LineStats, bool)>) -> Vec<Node> {
    let mut builder = Builder::default();
    for (path, (change, stats, staged)) in files {
        builder.insert(&path, change, stats, staged);
    }
    builder.into_nodes("")
}
