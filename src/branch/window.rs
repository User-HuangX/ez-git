//! 分支窗口：选择只是查看；Git 操作在后台执行，冲突保留给用户继续或中止。

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::auto_pull;
use super::{BranchInfo, Choice, Operation, Progress, Snapshot};
use crate::config::{AutoPullConfig, DEFAULT_AUTO_PULL_INTERVAL_SECS, RepoConfig, current_repo};
use crate::repo::git_get_repo;
use crate::status::ui::StatusTreeHandle;

#[derive(Default)]
struct BranchWindows(BTreeMap<PathBuf, AnyWindowHandle>);
impl Global for BranchWindows {}

/// 每个仓库只打开一个分支窗口，重复点击只聚焦，不切换旧窗口的操作对象。
pub(crate) fn open(cx: &mut App) -> Result<()> {
    auto_pull::init(cx);
    let repo = git_get_repo(current_repo()?)?;
    let path = repo
        .workdir()
        .context("裸仓库没有可操作的工作目录")?
        .canonicalize()?;
    if let Some(handle) = cx
        .try_global::<BranchWindows>()
        .and_then(|windows| windows.0.get(&path))
        .copied()
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return Ok(());
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(200.), px(120.)),
            size: size(px(980.), px(740.)),
        })),
        window_min_size: Some(size(px(820.), px(580.))),
        titlebar: Some(TitlebarOptions {
            title: Some("分支管理".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let window_path = path.clone();
    let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| BranchView::new(window_path, window, cx))
    })?;
    if !cx.has_global::<BranchWindows>() {
        cx.set_global(BranchWindows::default());
    }
    cx.global_mut::<BranchWindows>().0.insert(path, handle);
    Ok(())
}

struct BranchView {
    path: PathBuf,
    state: Option<Snapshot>,
    selected: Option<String>,
    remote: Option<String>,
    name: Entity<InputState>,
    period: Entity<InputState>,
    auto_settings: Option<AutoPullConfig>,
    auto_revision: u64,
    busy: Option<&'static str>,
    result: Option<std::result::Result<String, String>>,
    _input: Subscription,
    _period: Subscription,
    _timer: Subscription,
}

impl BranchView {
    fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("例如 feature/login"));
        let input = cx.observe(&name, |_, _, cx| cx.notify());
        let period = cx.new(|cx| InputState::new(window, cx).placeholder("300"));
        let period_subscription = cx.observe(&period, |_, _, cx| cx.notify());
        let timer_subscription =
            cx.observe_global_in::<auto_pull::AutoPull>(window, |this, window, cx| {
                let completed = cx
                    .try_global::<auto_pull::AutoPull>()
                    .filter(|auto| auto.path.as_ref() == Some(&this.path))
                    .map(|auto| (auto.revision, auto.running));
                if let Some((revision, false)) = completed
                    && revision != this.auto_revision
                {
                    this.auto_revision = revision;
                    if this.busy.is_none() {
                        this.start(Operation::Refresh, window, cx);
                    }
                }
                cx.notify();
            });
        let mut this = Self {
            path,
            state: None,
            selected: None,
            remote: None,
            name,
            busy: None,
            result: None,
            _input: input,
            period,
            auto_settings: None,
            auto_revision: 0,
            _period: period_subscription,
            _timer: timer_subscription,
        };
        this.start(Operation::Refresh, window, cx);
        this
    }

    fn start(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(match operation {
            Operation::Refresh => "读取分支",
            Operation::Create(_) => "新建分支",
            Operation::Fetch(_) => "Fetch",
            Operation::Pull => "Pull",
            Operation::Switch(_) => "Switch",
            Operation::Merge(_) => "Merge",
            Operation::Rebase(_) => "Rebase",
            Operation::SetUpstream(_) => "设置上游",
            Operation::Resolve {
                side: Choice::Manual,
                ..
            } => "标记已解决",
            Operation::Resolve { .. } => "取一侧内容",
            Operation::Continue => "继续",
            Operation::Abort => "中止",
            Operation::Skip => "跳过当前提交",
        });
        self.result = None;
        cx.notify();
        let path = self.path.clone();
        let expected = self.state.clone();
        let create = matches!(operation, Operation::Create(_));
        let refresh_main = !matches!(operation, Operation::Refresh);
        let task = cx.background_executor().spawn(async move {
            let result = match expected {
                Some(expected) => super::execute(&path, &operation, &expected),
                None if matches!(operation, Operation::Refresh) => Ok("分支列表已刷新".to_string()),
                None => Err(anyhow::anyhow!("分支状态尚未加载，请刷新后重试")),
            };
            // 即使操作失败，merge/rebase 也可能留下冲突，必须显示真实的新状态。
            let state = super::snapshot(&path);
            let settings = RepoConfig::load(&path).map(|config| config.auto_pull);
            (result, state, settings)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (result, state, settings) = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                if create && result.is_ok() {
                    this.name
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                this.result = Some(result.map_err(|err| format!("{err:#}")));
                if this.auto_settings.is_none() {
                    match settings {
                        Ok(settings) => {
                            let seconds = settings
                                .interval()
                                .unwrap_or(DEFAULT_AUTO_PULL_INTERVAL_SECS);
                            this.period.update(cx, |state, cx| {
                                state.set_value(seconds.to_string(), window, cx)
                            });
                            this.auto_settings = Some(settings);
                        }
                        Err(err) => this.result = Some(Err(format!("读取定时设置失败：{err:#}"))),
                    }
                }
                match state {
                    Ok(state) => {
                        if !state
                            .branches
                            .iter()
                            .any(|branch| Some(&branch.reference) == this.selected.as_ref())
                        {
                            this.selected = state
                                .current
                                .as_ref()
                                .map(|name| format!("refs/heads/{name}"))
                                .filter(|reference| {
                                    state
                                        .branches
                                        .iter()
                                        .any(|branch| &branch.reference == reference)
                                })
                                .or_else(|| {
                                    state
                                        .branches
                                        .first()
                                        .map(|branch| branch.reference.clone())
                                });
                        }
                        if !state
                            .remotes
                            .iter()
                            .any(|remote| Some(remote) == this.remote.as_ref())
                        {
                            this.remote = state
                                .remotes
                                .iter()
                                .find(|name| name.as_str() == "origin")
                                .or_else(|| state.remotes.first())
                                .cloned();
                        }
                        this.state = Some(state);
                    }
                    Err(err) => {
                        this.state = None;
                        this.result = Some(Err(format!("读取仓库状态失败：{err:#}")));
                    }
                }
                if refresh_main
                    && current_repo()
                        .is_ok_and(|path| path.canonicalize().ok().as_ref() == Some(&this.path))
                    && let Some(handle) = cx.try_global::<StatusTreeHandle>().cloned()
                {
                    handle.refresh(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_auto_pull(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| -> Result<AutoPullConfig> {
            let previous = RepoConfig::load(&self.path)?.auto_pull;
            // 停用不能被尚未输入完成的周期阻挡，也不能取消已经开始的 Git 操作。
            let seconds = if previous.enabled && !enabled {
                previous
                    .interval()
                    .unwrap_or(DEFAULT_AUTO_PULL_INTERVAL_SECS)
            } else {
                AutoPullConfig::parse_interval(self.period.read(cx).value().as_ref())?
            };
            let settings = AutoPullConfig {
                enabled,
                interval_secs: seconds,
            };
            RepoConfig::save_auto_pull(&self.path, settings)?;
            Ok(settings)
        })();
        match result {
            Ok(settings) => {
                self.auto_settings = Some(settings);
                self.period.update(cx, |state, cx| {
                    state.set_value(settings.interval_secs.to_string(), window, cx)
                });
                self.result = Some(Ok(if enabled {
                    format!("自动 Pull 已开启，每 {} 秒检查一次", settings.interval_secs)
                } else {
                    "自动 Pull 已关闭（正在执行的操作会安全完成）".to_string()
                }));
            }
            Err(err) => self.result = Some(Err(format!("保存定时设置失败：{err:#}"))),
        }
        cx.notify();
    }

    fn auto_pull_panel(&self, cx: &mut Context<Self>) -> Div {
        let enabled = self.auto_settings.is_some_and(|settings| settings.enabled);
        let disabled = self.auto_settings.is_none() || self.busy.is_some();
        v_flex().child(
            GroupBox::new()
                .id("automatic-pull-group")
                .outline()
                .title(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::Clock3).size_4())
                        .child("定时 Pull"),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child("执行周期（秒）"))
                        .child(
                            Input::new(&self.period)
                                .aria_label("自动 Pull 周期，单位秒")
                                .disabled(disabled),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("10～86400 秒，默认 300 秒"),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("save-auto-pull-period")
                                .label("保存周期")
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.save_auto_pull(enabled, window, cx)
                                })),
                        )
                        .child(
                            Button::new("toggle-auto-pull")
                                .label(if enabled {
                                    "停用自动 Pull"
                                } else {
                                    "开启自动 Pull"
                                })
                                .toggled(enabled)
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.save_auto_pull(!enabled, window, cx)
                                })),
                        ),
                )
                .child(div().text_sm().child(auto_pull::status(cx, &self.path)))
                .footer("仅当前仓库、干净且有上游时执行。关闭窗口仍运行；停用不打断已开始的操作。"),
        )
    }
    fn confirm(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        let text = match &operation {
            Operation::Skip => {
                "跳过变基中当前正在重放的提交，它的改动不会进入新分支历史；后续提交仍会继续重放。"
                    .to_string()
            }
            Operation::Rebase(reference) => format!(
                "把当前分支变基到 {reference}。这会重写当前分支独有的提交；已推送的分支请谨慎操作。不会强制推送。"
            ),
            Operation::Abort => {
                "中止当前合并/变基并恢复操作前的状态。本次冲突解决的改动会被丢弃。".to_string()
            }
            _ => return self.start(operation, window, cx),
        };
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let operation = operation.clone();
            dialog.title("确认分支操作").child(text.clone()).footer(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-branch-action")
                            .label("取消")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("confirm-branch-action")
                            .primary()
                            .label("确认执行")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                view.update(cx, |view, cx| {
                                    view.start(operation.clone(), window, cx)
                                });
                            }),
                    ),
            )
        });
    }

    fn action_button(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        operation: Operation,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .label(label)
            .disabled(self.busy.is_some() || auto_pull::running(cx) || !enabled)
            .on_click(
                cx.listener(move |this, _, window, cx| this.confirm(operation.clone(), window, cx)),
            )
    }

    fn branch_row(&self, index: usize, branch: &BranchInfo, cx: &mut Context<Self>) -> Button {
        let current = self
            .state
            .as_ref()
            .and_then(|state| state.current.as_ref())
            .is_some_and(|name| !branch.remote && name == &branch.name);
        let reference = branch.reference.clone();
        Button::new(("branch-row", index))
            .ghost()
            .icon(if branch.remote {
                IconName::CloudDownload
            } else {
                IconName::GitBranch
            })
            .w_full()
            .justify_start()
            .overflow_hidden()
            .label(if current {
                format!("{}（当前）", branch.name)
            } else {
                branch.name.clone()
            })
            .toggled(self.selected.as_ref() == Some(&branch.reference))
            .disabled(self.busy.is_some())
            .accessibility_label(format!("选择分支 {}", branch.name))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(reference.clone());
                // 选择只更新详情，不暗中 Switch，也不打开主窗口的 diff。
                cx.notify();
            }))
    }
}

impl Render for BranchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.as_ref();
        let selected = state.and_then(|state| {
            state
                .branches
                .iter()
                .find(|branch| Some(&branch.reference) == self.selected.as_ref())
        });
        let current = state
            .and_then(|state| state.current.as_deref())
            .unwrap_or("detached HEAD");
        let idle = state.is_some_and(|state| state.progress == Progress::Idle);
        let clean = state.is_some_and(|state| state.dirty == 0) && idle;
        let has_head = state.is_some_and(|state| state.head.is_some());
        let on_branch = state.is_some_and(|state| state.current.is_some());
        let target = self.selected.clone().unwrap_or_default();
        let same = selected.is_some_and(|branch| !branch.remote && branch.name == current);
        let integrate = clean && has_head && on_branch && selected.is_some() && !same;
        let recover =
            state.is_some_and(|state| matches!(state.progress, Progress::Merge | Progress::Rebase));
        let resolved = state.is_some_and(|state| state.conflicts.is_empty());
        let new_name = self.name.read(cx).value().trim().to_string();
        let selected_remote = self.remote.clone().unwrap_or_default();
        let muted = cx.theme().muted_foreground;
        let mut local_rows = Vec::new();
        let mut remote_rows = Vec::new();
        let mut remotes = Vec::new();
        let mut conflicts = Vec::new();
        if let Some(state) = state {
            for (index, branch) in state.branches.iter().enumerate() {
                let row = self.branch_row(index, branch, cx);
                if branch.remote {
                    remote_rows.push(row);
                } else {
                    local_rows.push(row);
                }
            }
            for (index, remote) in state.remotes.iter().enumerate() {
                let remote = remote.clone();
                remotes.push(
                    Button::new(("fetch-remote", index))
                        .ghost()
                        .label(remote.clone())
                        .toggled(self.remote.as_ref() == Some(&remote))
                        .disabled(self.busy.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remote = Some(remote.clone());
                            cx.notify();
                        })),
                );
            }
            for (index, path) in state.conflicts.iter().enumerate() {
                let mut resolve = |id: (&'static str, usize), label: &'static str, side: Choice| {
                    self.action_button(
                        id,
                        label,
                        Operation::Resolve {
                            file: path.clone(),
                            side,
                        },
                        true,
                        cx,
                    )
                };
                conflicts.push(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .flex_wrap()
                        .child(div().flex_1().min_w_0().child(path.clone()))
                        .child(resolve(("keep-ours", index), "用我的", Choice::Ours))
                        .child(resolve(("keep-theirs", index), "用他的", Choice::Theirs))
                        .child(resolve(
                            ("resolve-conflict", index),
                            "标记已解决",
                            Choice::Manual,
                        )),
                );
            }
        }
        let progress_text = state
            .map(|state| {
                format!(
                    "{} · 未提交路径 {} 个 · 冲突 {} 个",
                    state.progress.label(),
                    state.dirty,
                    state.conflicts.len()
                )
            })
            .unwrap_or_else(|| "正在读取仓库状态…".to_string());
        let upstream = state
            .and_then(|state| {
                state
                    .branches
                    .iter()
                    .find(|branch| !branch.remote && branch.name == current)
            })
            .and_then(|branch| branch.upstream.as_ref());
        let detail = selected
            .map(|branch| {
                let tracking = branch
                    .tracking
                    .map(|(ahead, behind)| format!(" · 领先 {ahead} / 落后 {behind}"))
                    .unwrap_or_default();
                format!(
                    "{}\n{} · {}\n上游：{}{}",
                    branch.name,
                    &branch.oid.to_string()[..8],
                    branch.summary,
                    branch.upstream.as_deref().unwrap_or("未设置"),
                    tracking
                )
            })
            .unwrap_or_else(|| "选择左侧分支查看详情；新仓库需先完成第一次提交。".to_string());
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });

        v_flex().size_full().bg(cx.theme().background).text_color(cx.theme().foreground)
            .child(h_flex().h(px(56.)).px_5().gap_3().items_center().bg(cx.theme().tokens.group_box)
                .border_b_1().border_color(cx.theme().border)
                .child(Icon::new(IconName::GitBranch).size_5().text_color(muted))
                .child(v_flex().flex_1().min_w_0().gap_0p5()
                    .child(div().font_medium().child("分支管理"))
                    .child(div().text_xs().text_color(muted).truncate().child(self.path.display().to_string())))
                .child(self.action_button("refresh-branches", "刷新", Operation::Refresh, true, cx))
                .when(client_decorations, |header| header.child(Button::new("close-branches").ghost()
                    .icon(IconName::Close).accessibility_label("关闭分支窗口").disabled(self.busy.is_some())
                    .on_click(|_, window, _| window.remove_window()))))
            .child(h_flex().flex_1().min_h_0().items_stretch()
                .child(v_flex().id("branch-list").w(px(260.)).min_h_0().flex_shrink_0().p_4().gap_2()
                    .bg(cx.theme().tokens.group_box).border_r_1().border_color(cx.theme().border).overflow_y_scroll()
                    .child(div().text_xs().font_medium().text_color(muted).child(format!("本地分支  {}", local_rows.len())))
                    .children(local_rows)
                    .child(div().mt_4().text_xs().font_medium().text_color(muted).child(format!("远程跟踪分支  {}", remote_rows.len())))
                    .children(remote_rows))
                .child(v_flex().id("branch-details").flex_1().min_w_0().min_h_0().p_5().gap_5().text_sm().overflow_y_scroll()
                    .child(v_flex().gap_1()
                        .child(div().font_medium().child(format!("当前分支：{current}")))
                        .child(div().text_xs().text_color(muted).child(progress_text)))
                    .when_some(self.busy, |panel, action| panel.child(div().text_sm().child(format!("正在执行 {action}…"))))
                    .when_some(self.result.clone(), |panel, result| panel.child(match result {
                        Ok(message) => Alert::success("branch-result", message),
                        Err(message) => Alert::error("branch-result", message),
                    }))
                    .child(GroupBox::new().id("branch-target-group").outline().title("选择的分支")
                        .child(div().text_sm().child(detail))
                        .child(h_flex().gap_2().flex_wrap()
                            .child(self.action_button("switch-branch", "Switch 切换", Operation::Switch(target.clone()), clean && selected.is_some_and(|branch| !branch.remote) && !same, cx))
                            .child(self.action_button("merge-branch", "Merge 合入当前", Operation::Merge(target.clone()), integrate, cx))
                            .child(self.action_button("rebase-branch", "Rebase 当前变基", Operation::Rebase(target.clone()), integrate, cx)))
                        .footer("目标分支合入当前分支；变基会重写当前分支的独有提交，执行前需确认。工作区须无未提交改动。"))
                    .child(GroupBox::new().id("branch-remotes-group").outline().title("远程同步")
                        // 没有远程时不渲染空行，避免分组顶部多出一段留白
                        .when(!remotes.is_empty(), |group| {
                            group.child(h_flex().gap_1().flex_wrap().children(remotes))
                        })
                        .child(div().text_sm().child(format!("上游：{}", upstream.map(String::as_str).unwrap_or("未设置"))))
                        .child(h_flex().gap_2().flex_wrap()
                            .child(self.action_button("fetch-branches", "Fetch 获取", Operation::Fetch(selected_remote), self.remote.is_some(), cx))
                            .child(self.action_button("pull-branch", "Pull 快进更新", Operation::Pull, clean && has_head && on_branch && upstream.is_some(), cx))
                            .child(self.action_button("set-upstream", "设为上游", Operation::SetUpstream(target), idle && on_branch && selected.is_some_and(|branch| branch.remote), cx)))
                        .footer("Fetch 更新远程引用，Pull 只快进。分叉时先选择上游分支，再合并或变基。"))
                    .child(self.auto_pull_panel(cx))
                    .child(GroupBox::new().id("branch-create-group").outline().title("新建本地分支")
                        .child(Input::new(&self.name).aria_label("新分支名称").disabled(self.busy.is_some() || !idle))
                        .child(self.action_button("create-branch", "从当前 HEAD 新建", Operation::Create(new_name.clone()), idle && has_head && !new_name.is_empty(), cx))
                        .footer("新建只创建分支，不自动切换。选择新分支后点击 Switch。"))
                    .when(recover, |panel| panel.child(GroupBox::new().id("branch-conflicts-group").outline().title("冲突与恢复")
                        .child(div().text_sm().child("「用我的 / 用他的」直接取那一侧的内容并暂存；手动改完文件后点「标记已解决」。Merge 时 ours 是当前分支、theirs 是合入分支，Rebase 时相反。继续可能遇到下一批冲突；中止会丢弃本次解决改动。"))
                        .children(conflicts)
                        .child(h_flex().gap_2().flex_wrap()
                            .child(self.action_button("continue-branch-op", "继续操作", Operation::Continue, resolved, cx))
                            .when(state.is_some_and(|state| state.progress == Progress::Rebase), |actions| actions.child(self.action_button("skip-rebase-commit", "跳过当前提交…", Operation::Skip, true, cx)))
                            .child(self.action_button("abort-branch-op", "中止操作…", Operation::Abort, true, cx)))))))
    }
}
