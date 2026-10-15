//! 主窗口的组装与布局：工具栏、状态树、文件 diff。

use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::config::Config;
use crate::repo::open_or_init;
use crate::status::diff::DiffView;
use crate::status::ui::{StatusTree, StatusTreeHandle};

mod toolbar;

use toolbar::Toolbar;

struct TreeExpanded(bool);

impl Global for TreeExpanded {}

fn tree_expanded(cx: &App) -> bool {
    cx.try_global::<TreeExpanded>().is_none_or(|state| state.0)
}

/// 提交模式：左侧只列有改动的文件，打钩即暂存，提交时只提交打钩的那些。
struct CommitMode(bool);

impl Global for CommitMode {}

pub(crate) fn commit_mode(cx: &App) -> bool {
    cx.try_global::<CommitMode>().is_some_and(|state| state.0)
}

pub(super) fn set_commit_mode(enabled: bool, cx: &mut App) {
    cx.set_global(CommitMode(enabled));
}

/// 启动时决定打开哪个仓库：优先历史里最近打开的一个，路径不在了才退回运行目录；
/// 目录还不是 Git 仓库就初始化。失败留状态树显示，不在这儿拦一道。
fn open_startup_repo() {
    let recent = Config::get_config()
        .ok()
        .and_then(|config| config.current_repository())
        .filter(|path| path.is_dir());
    let Some(path) = recent.or_else(|| std::env::current_dir().ok()) else {
        return;
    };

    if open_or_init(&path).is_ok()
        && let Err(err) = Config::record_open(&path)
    {
        eprintln!("记录打开历史失败：{err:#}");
    }
}

pub(crate) struct Workspace {
    toolbar: Entity<Toolbar>,
    tree: Entity<StatusTree>,
    diff: Entity<DiffView>,
}

impl Workspace {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.set_global(TreeExpanded(true));
        open_startup_repo();
        let diff = cx.new(|cx| DiffView::new(window, cx));
        let tree = cx.new(|cx| StatusTree::new(window, diff.clone(), cx));
        // 提交后通过弱句柄刷新，避免提交模块依赖整个工作区。
        cx.set_global(StatusTreeHandle::new(&tree));
        let toolbar = cx.new(|cx| Toolbar::new(tree.clone(), cx));
        crate::branch::auto_pull::init(cx);
        Self {
            toolbar,
            tree,
            diff,
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(self.toolbar.clone())
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("workspace-panels")
                        .when(tree_expanded(cx), |this| {
                            this.child(
                                resizable_panel()
                                    .size(px(320.))
                                    .size_range(px(220.)..px(900.))
                                    .child(self.tree.clone()),
                            )
                        })
                        .child(resizable_panel().child(self.diff.clone())),
                ),
            )
    }
}
