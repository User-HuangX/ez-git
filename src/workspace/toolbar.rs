//! 主窗口工具栏：左侧仓库入口和仓库操作，右侧增删合计。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex};
use gpui_kit::*;

use super::{TreeExpanded, commit_mode, set_commit_mode, tree_expanded};
use crate::status::ui::{StatusTree, line_stats};
use crate::{config, sync};

/// 原生 tooltip 由 GPUI window 绘制，不依赖组件的覆盖层。
fn tooltipped(id: &'static str, tooltip: SharedString, button: Button) -> impl IntoElement {
    div()
        .id(id)
        .child(button)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

pub(super) struct Toolbar {
    tree: Entity<StatusTree>,
    _tree: Subscription,
}

impl Toolbar {
    pub(super) fn new(tree: Entity<StatusTree>, cx: &mut Context<Self>) -> Self {
        // 刷新、提交或换仓库改变树时，工具栏的仓库名和统计一起重绘。
        let subscription = cx.observe(&tree, |_, _, cx| cx.notify());
        Self {
            tree,
            _tree: subscription,
        }
    }
}

impl Render for Toolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (repo, repo_path, branch) = {
            let tree = self.tree.read(cx);
            (
                tree.repo_label(),
                tree.repo_path_label(),
                tree.branch_label().to_string(),
            )
        };
        h_flex()
            .w_full()
            .h(px(56.))
            .px_3()
            .gap_2()
            .items_center()
            .bg(cx.theme().tokens.group_box)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(tooltipped(
                        "展开/收起目录树",
                        "展开/收起目录树".into(),
                        Button::new("toggle-tree")
                            .ghost()
                            .icon(if tree_expanded(cx) {
                                IconName::PanelLeftClose
                            } else {
                                IconName::PanelLeftOpen
                            })
                            .accessibility_label("展开/收起目录树")
                            .on_click(|_, window, cx| {
                                cx.set_global(TreeExpanded(!tree_expanded(cx)));
                                window.refresh();
                            }),
                    ))
                    .child(tooltipped(
                        "刷新",
                        "刷新工作区".into(),
                        Button::new("refresh")
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .label("刷新")
                            .accessibility_label("刷新")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.tree.update(cx, |tree, cx| tree.refresh(cx))
                            })),
                    )),
            )
            .child(div().mx_1().w(px(1.)).h_5().bg(cx.theme().border))
            // 仓库入口放在视图控制之后：最左边先给「展开/收起 + 刷新」
            .child(
                div()
                    .id("current-repository")
                    .min_w_0()
                    .child(config::ui::repo_button(repo))
                    .tooltip(move |window, cx| Tooltip::new(repo_path.clone()).build(window, cx)),
            )
            .child(div().mx_1().w(px(1.)).h_5().bg(cx.theme().border))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(tooltipped(
                        "分支",
                        "分支管理：Fetch / Pull / Switch / Merge / Rebase".into(),
                        Button::new("branches")
                            .ghost()
                            .icon(IconName::GitBranch)
                            .label(branch)
                            .max_w(px(220.))
                            .overflow_hidden()
                            .accessibility_label("打开分支管理窗口")
                            .on_click(|_, window, cx| {
                                if let Err(err) = crate::branch::window::open(cx) {
                                    window.push_notification(
                                        Notification::error(format!("打开分支窗口失败：{err:#}")),
                                        cx,
                                    );
                                }
                            }),
                    ))
                    .child(tooltipped(
                        "提交",
                        "提交：选择文件、暂存与创建提交".into(),
                        Button::new("commit")
                            .ghost()
                            .icon(IconName::GitCommitHorizontal)
                            .label("提交")
                            .toggled(commit_mode(cx))
                            .accessibility_label("提交")
                            .on_click(cx.listener(|this, _, window, cx| {
                                set_commit_mode(!commit_mode(cx), cx);
                                this.tree.update(cx, |tree, cx| tree.refresh(cx));
                                window.refresh();
                            })),
                    ))
                    .child(tooltipped(
                        "推送",
                        "推送到当前分支的上游".into(),
                        sync::ui::push_button().label("推送"),
                    )),
            )
            .child(
                h_flex()
                    .ml_auto()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("更改"),
                    )
                    .child(line_stats(self.tree.read(cx).line_stats())),
            )
    }
}
