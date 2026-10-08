//! 左侧目录树：改动颜色、文件的增删行数和选中状态。
//!
//! 使用 TreeState 的虚拟滚动和键盘导航，选中文件时更新独立的 diff 面板。
//! ponytail: 刷新同步读取仓库；大型仓库出现卡顿时再移到后台。

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::diff::DiffView;
use crate::config::current_repo;
use crate::repo::git_get_repo;
use crate::status::{self, Change, LineStats, Node, git_file_diff};

const GREEN: u32 = 0x4ade80;
const YELLOW: u32 = 0xfacc15;
const RED: u32 = 0xf87171;
const CLEAN: u32 = 0xb0b0c0;
const MUTED: u32 = 0x8888aa;

/// 改动类型对应的颜色。
fn color_of(change: Change) -> u32 {
    match change {
        Change::Added => GREEN,
        Change::Modified => YELLOW,
        Change::Deleted => RED,
        Change::Clean => CLEAN,
    }
}

/// 树条目和工具栏共用的增删统计：为 0 的一侧不显示。
pub(crate) fn line_stats(stats: LineStats) -> Div {
    h_flex()
        .gap_2()
        .text_xs()
        .flex_shrink_0()
        .when(stats.added > 0, |this| {
            this.child(
                h_flex()
                    .gap_0p5()
                    .text_color(rgb(GREEN))
                    .child(Icon::new(IconName::ArrowUp).size_3())
                    .child(format!("+{}", stats.added)),
            )
        })
        .when(stats.removed > 0, |this| {
            this.child(
                h_flex()
                    .gap_0p5()
                    .text_color(rgb(RED))
                    .child(Icon::new(IconName::ArrowDown).size_3())
                    .child(format!("-{}", stats.removed)),
            )
        })
}

/// 给别的模块用的句柄：提交之后让状态树重新读一遍仓库。
#[derive(Clone)]
pub(crate) struct StatusTreeHandle(WeakEntity<StatusTree>);

impl Global for StatusTreeHandle {}

impl StatusTreeHandle {
    pub(crate) fn new(tree: &Entity<StatusTree>) -> Self {
        Self(tree.downgrade())
    }

    /// 重新读仓库；树和右边 diff 一起更新。
    pub(crate) fn refresh(&self, cx: &mut App) {
        self.0.update(cx, |tree, cx| tree.refresh(cx)).ok();
    }
}

pub(crate) struct StatusTree {
    /// gpui-kit 的树状态：条目、展开、选中、滚动都在它里面。
    state: Entity<TreeState>,
    /// 选中的条目推给它显示
    diff: Entity<DiffView>,
    /// 当前打开的仓库；读取失败时为 None，工具栏据此显示。
    repo: Option<PathBuf>,
    nodes: Vec<Node>,
    /// 节点 id → 改动类型和行数；目录不参与，行数只在文件和工具栏合计里出现。
    changes: Rc<HashMap<SharedString, (Change, LineStats)>>,
    /// 当前选中的条目（路径，是不是目录）；刷新后用来恢复选中
    selected: Option<(String, bool)>,
    error: Option<SharedString>,
    _state: Subscription,
}

impl StatusTree {
    pub(crate) fn new(diff: Entity<DiffView>, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| TreeState::new(cx));
        let state_subscription =
            cx.observe(&state, |this, state, cx| this.on_selection(&state, cx));

        let mut this = Self {
            state,
            diff,
            repo: None,
            nodes: Vec::new(),
            changes: Rc::new(HashMap::new()),
            selected: None,
            error: None,
            _state: state_subscription,
        };
        this.refresh(cx);
        this
    }

    pub(crate) fn line_stats(&self) -> LineStats {
        self.nodes.iter().map(Node::line_stats).sum()
    }

    /// 工具栏按钮上的仓库名。
    pub(crate) fn repo_label(&self) -> String {
        self.repo
            .as_deref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "选择仓库".into())
    }

    /// 工具栏 tooltip 里的完整路径。
    pub(crate) fn repo_path_label(&self) -> String {
        match &self.repo {
            Some(path) => path.display().to_string(),
            None => "点这里打开目录历史和配置".into(),
        }
    }

    /// 重新读一遍仓库（工具栏「刷新」、提交和换仓库之后都走这里）。
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        let loaded = current_repo().and_then(|path| {
            let repo = git_get_repo(&path)?;
            Ok((path, status::tree(&repo)?))
        });
        match loaded {
            Ok((path, nodes)) => {
                self.repo = Some(path);
                self.nodes = nodes;
            }
            Err(err) => {
                self.repo = None;
                self.nodes.clear();
                self.error = Some(format!("{err:#}").into());
                self.selected = None;
            }
        }

        let mut changes = HashMap::new();
        let items = build_items(&self.nodes, &mut changes);
        self.changes = Rc::new(changes);

        let previous = self.selected.clone();
        self.state
            .update(cx, |state, cx| state.set_items(items, cx));
        // set_items 会清空选中，能找回来就找回来
        if let Some((path, _)) = previous {
            let index = self.state.read(cx).index_of(&SharedString::from(path));
            if let Some(index) = index {
                self.state
                    .update(cx, |state, cx| state.set_selected_index(Some(index), cx));
            }
        }

        self.show_selected(cx);
        cx.notify();
    }

    /// 树里换了选中项：右边的 diff 跟着换。清空选中（刷新导致）不处理。
    fn on_selection(&mut self, state: &Entity<TreeState>, cx: &mut Context<Self>) {
        let selected = state
            .read(cx)
            .selected_entry()
            .map(|entry| (entry.item().id.to_string(), entry.is_folder()));
        let Some(selected) = selected else {
            return;
        };
        if self.selected.as_ref() == Some(&selected) {
            return;
        }

        self.selected = Some(selected);
        self.show_selected(cx);
        cx.notify();
    }

    /// 把选中项交给右侧面板：文件直接算好 diff；目录不算，那等于把目录下所有
    /// 文件的全量 diff 一次塞进编辑器。
    fn show_selected(&mut self, cx: &mut Context<Self>) {
        let Some((path, is_folder)) = self.selected.clone() else {
            return self
                .diff
                .update(cx, |diff, cx| diff.show(None, false, Ok(Vec::new()), cx));
        };
        if is_folder {
            return self.diff.update(cx, |diff, cx| {
                diff.show(Some(path), true, Ok(Vec::new()), cx)
            });
        }

        let lines = current_repo()
            .and_then(|repo| git_get_repo(&repo))
            .and_then(|repo| git_file_diff(&repo, &path));
        self.diff
            .update(cx, |diff, cx| diff.show(Some(path), false, lines, cx));
    }
}

/// 把目录树转成组件条目；只有文件带颜色和增删行数。
fn build_items(
    nodes: &[Node],
    changes: &mut HashMap<SharedString, (Change, LineStats)>,
) -> Vec<TreeItem> {
    nodes
        .iter()
        .map(|node| match node {
            Node::Dir {
                path,
                name,
                children,
                ..
            } => TreeItem::new(path.clone(), name.clone())
                .expanded(true)
                .children(build_items(children, changes)),
            Node::File {
                path,
                name,
                change,
                stats,
            } => {
                changes.insert(SharedString::from(path.clone()), (*change, *stats));
                TreeItem::new(path.clone(), name.clone())
            }
        })
        .collect()
}

impl Render for StatusTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let changes = self.changes.clone();
        let error = self.error.clone();

        v_flex()
            .h_full()
            .w(px(260.))
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(RED))
                        .child(error),
                )
            })
            .child(
                tree(&self.state, move |ix, entry, selected, _window, _cx| {
                    let item = entry.item();
                    let (change, stats) = changes
                        .get(&item.id)
                        .copied()
                        .unwrap_or((Change::Clean, LineStats::default()));
                    let (icon, color) = if entry.is_folder() {
                        let icon = if entry.is_expanded() {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        };
                        (icon, MUTED)
                    } else {
                        (IconName::File, color_of(change))
                    };

                    // 目录不显示增删统计；文件只在有增减时显示。
                    let counts = match (entry.is_folder(), stats.added, stats.removed) {
                        (true, ..) | (false, 0, 0) => None,
                        (false, added, 0) => Some(format!("{}，新增 {added} 行", item.label)),
                        (false, 0, removed) => Some(format!("{}，删除 {removed} 行", item.label)),
                        (false, added, removed) => Some(format!(
                            "{}，新增 {added} 行，删除 {removed} 行",
                            item.label
                        )),
                    };

                    ListItem::new(ix)
                        .selected(selected)
                        .accessibility_label(
                            counts
                                .clone()
                                .unwrap_or_else(|| item.label.to_string()),
                        )
                        .pl(px(16.) * entry.depth() + px(12.))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_2()
                                .text_sm()
                                .text_color(rgb(color))
                                .child(icon)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(item.label.clone()),
                                )
                                .when(counts.is_some(), |this| {
                                    this.child(line_stats(stats))
                                }),
                        )
                })
                .size_full(),
            )
    }
}
