//! 左侧目录树：改动颜色、文件的增删行数和选中状态。
//!
//! 使用 TreeState 的虚拟滚动和键盘导航，选中文件时更新独立的 diff 面板。
//! ponytail: 刷新同步读取仓库；大型仓库出现卡顿时再移到后台。

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tree::{TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::diff::DiffView;
use super::file_icons::icon_for;
use crate::config::current_repo;
use crate::repo::git_get_repo;
use crate::status::{self, Change, LineStats, Node, git_file_diff};

const GREEN: u32 = 0x4ade80;
const YELLOW: u32 = 0xfacc15;
const RED: u32 = 0xf87171;
const CLEAN: u32 = 0xb0b0c0;

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
    branch: String,
    nodes: Vec<Node>,
    /// 节点 id → 改动类型、行数和是否已暂存；目录不参与，行数只在文件和工具栏合计里出现。
    changes: Rc<HashMap<SharedString, (Change, LineStats, bool)>>,
    /// 当前选中的条目（路径，是不是目录）；刷新后用来恢复选中
    selected: Option<(String, bool)>,
    error: Option<SharedString>,
    /// 提交模式栏里的提交说明。
    message: Entity<TextareaState>,
    _state: Subscription,
}

impl StatusTree {
    pub(crate) fn new(window: &mut Window, diff: Entity<DiffView>, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| TreeState::new(cx));
        let state_subscription =
            cx.observe(&state, |this, state, cx| this.on_selection(&state, cx));

        let mut this = Self {
            state,
            diff,
            repo: None,
            branch: "未打开仓库".to_string(),
            nodes: Vec::new(),
            changes: Rc::new(HashMap::new()),
            selected: None,
            error: None,
            message: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(3)
                    .placeholder("提交说明，第一行是标题")
            }),
            _state: state_subscription,
        };
        this.refresh(cx);
        this
    }

    pub(crate) fn line_stats(&self) -> LineStats {
        self.nodes.iter().map(Node::line_stats).sum()
    }

    pub(crate) fn branch_label(&self) -> &str {
        &self.branch
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
        self.reload(cx);
        // 内容变了，右侧 diff 也要重算
        self.show_selected(cx);
    }

    /// 只重读左边树的条目和勾选状态。暂存不动工作区，所以不用重算 diff。
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        let loaded = current_repo().and_then(|path| {
            let repo = git_get_repo(&path)?;
            let branch = repo
                .find_reference("HEAD")?
                .symbolic_target()?
                .and_then(|name| name.strip_prefix("refs/heads/"))
                .unwrap_or("detached HEAD")
                .to_string();
            Ok((path, branch, status::tree(&repo)?))
        });
        match loaded {
            Ok((path, branch, nodes)) => {
                self.branch = branch;
                self.repo = Some(path);
                self.nodes = nodes;
            }
            Err(err) => {
                self.repo = None;
                self.branch = "未打开仓库".to_string();
                self.nodes.clear();
                self.error = Some(format!("{err:#}").into());
                self.selected = None;
            }
        }

        let mut changes = HashMap::new();
        let items = build_items(&self.nodes, &mut changes, crate::workspace::commit_mode(cx));
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

        // diff 在后台算：仓库大或者文件大时，点一下不该卡住界面。
        // 算完如果用户已经选了别的文件，这次结果直接丢掉（永远只有最新的赢）。
        let clicked = std::time::Instant::now();
        let trace = std::env::var_os("EZ_GIT_TRACE").is_some();
        let task = cx.background_executor().spawn({
            let path = path.clone();
            async move {
                let started = std::time::Instant::now();
                let lines = current_repo()
                    .and_then(|repo| git_get_repo(&repo))
                    .and_then(|repo| git_file_diff(&repo, &path));
                (lines, started.elapsed())
            }
        });
        cx.spawn(async move |this, cx| {
            let (lines, git_time) = task.await;
            if trace {
                eprintln!(
                    "[diff] {path}：后台算 diff {git_time:?}（开仓库+diff），点击到拿到结果 {:?}",
                    clicked.elapsed()
                );
            }
            this.update(cx, |this, cx| {
                if this
                    .selected
                    .as_ref()
                    .is_some_and(|(selected, _)| selected == &path)
                {
                    this.diff
                        .update(cx, |diff, cx| diff.show(Some(path), false, lines, cx));
                }
            })
            .ok();
        })
        // 任务被 drop 就会取消，所以必须 detach：diff 要算完
        .detach();
    }

    /// 提交模式里点打钩：勾上加进暂存区，取消就从暂存区退回。
    fn toggle_stage(&mut self, path: String, staged: bool, cx: &mut Context<Self>) {
        let Some(repo_path) = self.repo.clone() else {
            return;
        };
        let paths = [path];
        let result = git_get_repo(&repo_path).and_then(|repo| match staged {
            true => crate::commit::git_stage(&repo, &paths),
            false => crate::commit::git_unstage(&repo, &paths),
        });
        let error: Option<SharedString> = result.err().map(|err| format!("{err:#}").into());
        // 勾选状态、行数、右边 diff 都要跟着变
        self.reload(cx);
        if let Some(error) = error {
            self.error = Some(error);
        }
    }

    /// 提交模式栏上的两个动作：只提交打钩的（暂存区），或者把工作区全部提交。
    fn commit(&mut self, staged_only: bool, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.message.read(cx).value().trim().to_string();
        let result = match staged_only {
            true => crate::commit::commit_staged(&message),
            false => crate::commit::commit_all(&message),
        };
        match result {
            Ok(oid) => {
                self.message
                    .update(cx, |state, cx| state.set_value("", window, cx));
                // 提交完树、行数、diff 都要重读
                self.refresh(cx);
                let short = &oid.to_string()[..8];
                window.push_notification(Notification::success(format!("已提交 {short}")), cx);
            }
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                cx.notify();
            }
        }
    }
}

/// 把目录树转成组件条目；只有文件带颜色和增删行数。
///
/// 提交模式下只留改动过的文件（未修改的连空目录一起隐藏），文件前面带打钩的复选框。
fn build_items(
    nodes: &[Node],
    changes: &mut HashMap<SharedString, (Change, LineStats, bool)>,
    commit_mode: bool,
) -> Vec<TreeItem> {
    nodes
        .iter()
        .filter_map(|node| match node {
            Node::Dir {
                path,
                name,
                children,
                ..
            } => {
                let children = build_items(children, changes, commit_mode);
                if commit_mode && children.is_empty() {
                    return None;
                }
                Some(
                    TreeItem::new(path.clone(), name.clone())
                        .expanded(true)
                        .children(children),
                )
            }
            Node::File {
                path,
                name,
                change,
                stats,
                staged,
            } => {
                if commit_mode && *change == Change::Clean {
                    return None;
                }
                changes.insert(SharedString::from(path.clone()), (*change, *stats, *staged));
                Some(TreeItem::new(path.clone(), name.clone()))
            }
        })
        .collect()
}

impl Render for StatusTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let commit_mode = crate::workspace::commit_mode(cx);
        let staged = self
            .changes
            .values()
            .filter(|(_, _, staged)| *staged)
            .count();
        let total = self.changes.len();
        let changed = self
            .changes
            .values()
            .filter(|(change, _, _)| *change != Change::Clean)
            .count();
        let store = cx.entity();
        let changes = self.changes.clone();
        let content = if changes.is_empty() {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .p_6()
                .gap_2()
                .child(
                    Icon::new(if commit_mode {
                        IconName::Check
                    } else {
                        IconName::FolderOpen
                    })
                    .size_8()
                    .text_color(cx.theme().muted_foreground),
                )
                .child(div().text_sm().font_medium().child(if commit_mode {
                    "工作区干净"
                } else {
                    "暂无文件"
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if commit_mode {
                            "没有需要暂存的改动"
                        } else {
                            "选择仓库后，文件将在这里显示"
                        }),
                )
                .into_any_element()
        } else {
            tree(&self.state, move |ix, entry, selected, _window, cx| {
                let item = entry.item();
                let (change, stats, staged) = changes.get(&item.id).copied().unwrap_or((
                    Change::Clean,
                    LineStats::default(),
                    false,
                ));
                let folder = entry.is_folder();
                let icon = if folder {
                    if entry.is_expanded() {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    }
                } else {
                    icon_for(&item.id)
                };
                let tone: Hsla = if folder || change == Change::Clean {
                    cx.theme().muted_foreground
                } else {
                    rgb(color_of(change)).into()
                };
                let change_name = match change {
                    Change::Added => "新增",
                    Change::Modified => "修改",
                    Change::Deleted => "删除",
                    Change::Clean => "未修改",
                };
                let counts = match (folder, stats.added, stats.removed) {
                    (true, ..) | (false, 0, 0) => None,
                    (false, added, removed) => Some(format!("新增 {added} 行，删除 {removed} 行")),
                };
                ListItem::new(ix)
                    .selected(selected)
                    .accessibility_label(format!(
                        "{}，{}，{}",
                        item.id,
                        change_name,
                        counts.as_deref().unwrap_or("")
                    ))
                    .pl(px(16.) * entry.depth() + px(8.))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .py_0p5()
                            .text_sm()
                            .when(commit_mode && !folder, |row| {
                                row.child(
                                    div()
                                        .id(format!("stage-box-{}", item.id))
                                        // Tree 在 mouse down 上选中行；打钩必须在此阶段阻止冒泡。
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(
                                            Checkbox::new(format!("stage-{}", item.id))
                                                .checked(staged)
                                                .accessibility_label(format!(
                                                    "{} 加入本次提交",
                                                    item.label
                                                ))
                                                .on_click({
                                                    let store = store.clone();
                                                    let path = item.id.to_string();
                                                    move |checked, _, cx| {
                                                        store.update(cx, |tree, cx| {
                                                            tree.toggle_stage(
                                                                path.clone(),
                                                                *checked,
                                                                cx,
                                                            )
                                                        });
                                                    }
                                                }),
                                        ),
                                )
                            })
                            .child(div().w(px(12.)).flex_shrink_0().when(folder, |slot| {
                                slot.child(
                                    Icon::new(if entry.is_expanded() {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .size_3()
                                    .text_color(cx.theme().muted_foreground),
                                )
                            }))
                            .child(Icon::new(icon).size_4().text_color(tone))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(cx.theme().foreground)
                                    .child(item.label.clone()),
                            )
                            .when(!folder && change != Change::Clean, |row| {
                                row.child(
                                    div()
                                        .px_1()
                                        .rounded_sm()
                                        .text_xs()
                                        .font_medium()
                                        .text_color(tone)
                                        .bg(tone.opacity(0.10))
                                        .child(match change {
                                            Change::Added => "A",
                                            Change::Modified => "M",
                                            Change::Deleted => "D",
                                            Change::Clean => "",
                                        }),
                                )
                            })
                            .when(counts.is_some(), |row| row.child(line_stats(stats))),
                    )
            })
            .flex_1()
            .min_h_0()
            .into_any_element()
        };

        v_flex()
            .h_full()
            .w(px(320.))
            .flex_shrink_0()
            .bg(cx.theme().tokens.group_box)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h_12()
                    .px_4()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Icon::new(if commit_mode {
                            IconName::GitCommitHorizontal
                        } else {
                            IconName::FolderGit2
                        })
                        .size_4()
                        .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_medium()
                            .child(if commit_mode {
                                "待提交文件"
                            } else {
                                "文件"
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if commit_mode {
                                format!("{staged} / {total} 已暂存")
                            } else {
                                format!("{changed} 项改动")
                            }),
                    ),
            )
            .when_some(self.error.clone(), |panel, error| {
                panel.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(content)
            .when(commit_mode, |panel| {
                panel.child(
                    v_flex()
                        .gap_3()
                        .p_4()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(div().flex_1().text_sm().font_medium().child("提交说明"))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("已暂存 {staged} 个文件")),
                                ),
                        )
                        .child(Textarea::new(&self.message).aria_label("提交说明"))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("commit-staged")
                                        .primary()
                                        .flex_1()
                                        .label("提交已暂存")
                                        .disabled(staged == 0)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.commit(true, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("commit-all")
                                        .flex_1()
                                        .label("提交全部")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.commit(false, window, cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("勾选仅暂存，点击文件名查看 Diff。"),
                        ),
                )
            })
    }
}
