//! 主窗口工具栏：左侧仓库入口和仓库操作，右侧增删合计。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, IconName, h_flex};
use gpui_kit::*;

use super::{TreeExpanded, tree_expanded};
use crate::status::ui::{StatusTree, line_stats};
use crate::{commit, config, sync};

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
        let (repo, repo_path) = {
            let tree = self.tree.read(cx);
            (tree.repo_label(), tree.repo_path_label())
        };

        h_flex()
            .w_full()
            .h_9()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            // 左上角的仓库入口：按钮上就是当前仓库。
            .child(
                div()
                    .id("current-repository")
                    .child(config::ui::repo_button(repo))
                    .tooltip(move |window, cx| Tooltip::new(repo_path.clone()).build(window, cx)),
            )
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
                "刷新".into(),
                Button::new("refresh")
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .accessibility_label("刷新")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tree.update(cx, |tree, cx| tree.refresh(cx))
                    })),
            ))
            .child(tooltipped("提交", "提交".into(), commit::ui::commit_button()))
            .child(tooltipped("推送", "推送".into(), sync::ui::push_button()))
            .child(line_stats(self.tree.read(cx).line_stats()).ml_auto().mr_2())
    }
}
