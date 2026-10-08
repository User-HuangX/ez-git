//! 配置窗口：打开历史（LRU）、选仓库目录 → 填作者、邮箱 → 存到该仓库的 `.git/ez-git.toml`。
//!
//! 目录走系统原生选择器；选中的目录还不是 Git 仓库就初始化，并记为当前仓库。
//! 作者、邮箱按仓库独立保存，不动全局 git 配置。

use std::path::{Path, PathBuf};

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{Config, DEFAULT_HISTORY_LIMIT, RepoConfig};
use crate::repo::open_or_init;
use crate::status::ui::StatusTreeHandle;

/// 配置窗口只保留一个：再点左上角的仓库按钮就把已有窗口提到前面。
struct ConfigWindow(AnyWindowHandle);

impl Global for ConfigWindow {}

/// 打开配置窗口；已有窗口仍存活时只激活它。
pub(super) fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<ConfigWindow>().map(|state| state.0) {
        // 窗口还活着就聚焦它；已经关掉则往下新建
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(240.), px(160.)),
            size: size(px(720.), px(360.)),
        })),
        titlebar: Some(TitlebarOptions {
            title: Some("配置".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| ConfigView::new(window, cx))
    }) {
        Ok((handle, _)) => cx.set_global(ConfigWindow(handle)),
        Err(err) => eprintln!("打开配置窗口失败：{err}"),
    }
}

enum Status {
    Idle,
    Ok(SharedString),
    Error(SharedString),
}

struct ConfigView {
    repo: Option<PathBuf>,
    /// 打开历史，最近使用的在前。
    history: Vec<PathBuf>,
    /// 历史上限，来自配置文件。
    history_limit: usize,
    author: Entity<InputState>,
    email: Entity<InputState>,
    status: Status,
}

impl ConfigView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            repo: None,
            history: Vec::new(),
            history_limit: DEFAULT_HISTORY_LIMIT,
            author: cx.new(|cx| InputState::new(window, cx).placeholder("例如 Hu Xin")),
            email: cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com")),
            status: Status::Idle,
        };

        // 打开就用上次的仓库和历史，省得每次重选。
        this.reload(window, cx);
        this
    }

    /// 从配置文件读打开历史和当前仓库；手工改过 config.toml 后点「刷新」也走这里。
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = match Config::get_config() {
            Ok(config) => config,
            Err(err) => {
                self.history.clear();
                return self.report(format!("{err:#}"), cx);
            }
        };
        self.history_limit = config.history_limit();
        self.history = config.history();

        let repo = self.repo.clone().or_else(|| config.current_repository());
        if let Some(repo) = repo {
            self.load_repo(repo, window, cx);
        }
        cx.notify();
    }

    fn report(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = Status::Error(message.into());
        cx.notify();
    }

    fn report_ok(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = Status::Ok(message.into());
        cx.notify();
    }

    /// 载入仓库身份（作者、邮箱），并记下当前仓库；调用前确保它已是 Git 仓库。
    fn load_repo(&mut self, repo: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // 没填过的字段会用 .git/config 里的 user.name / user.email 兜底
        let config = match RepoConfig::resolved(&repo) {
            Ok(config) => config,
            Err(err) => return self.report(format!("{err:#}"), cx),
        };

        self.author
            .update(cx, |state, cx| state.set_value(config.author, window, cx));
        self.email
            .update(cx, |state, cx| state.set_value(config.email, window, cx));
        self.report_ok(format!("当前仓库：{}", repo.display()), cx);
        self.repo = Some(repo);
    }

    /// 打开一个目录：不是 Git 仓库就初始化，记进历史，然后让状态树重读。
    fn open_dir(&mut self, repo: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let created = match open_or_init(&repo) {
            Ok((_, created)) => created,
            Err(err) => return self.report(format!("{err:#}"), cx),
        };
        if let Err(err) = Config::record_open(&repo) {
            return self.report(format!("记录打开历史失败：{err:#}"), cx);
        }
        self.load_repo(repo.clone(), window, cx);
        if created {
            self.report_ok(format!("{} 不是 Git 仓库，已初始化并打开", repo.display()), cx);
        }
        self.refresh_tree(cx);
    }

    /// 换仓库后状态树和 diff 重新读一遍。
    fn refresh_tree(&self, cx: &mut App) {
        if let Some(handle) = cx.try_global::<StatusTreeHandle>().cloned() {
            handle.refresh(cx);
        }
    }

    /// 历史里的一行：点一下切换到该目录。
    /// 返回具体元素类型，避免 `impl Trait` 捕获 `cx` 后逃出渲染闭包。
    fn history_row(&self, index: usize, repo: &Path, cx: &mut Context<Self>) -> Stateful<Div> {
        let current = self.repo.as_deref() == Some(repo);
        let path = repo.to_path_buf();
        h_flex()
            .id(("open-history-row", index))
            .w_full()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_color(if current {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .when(current, |this| this.bg(cx.theme().selection))
            .hover(|this| this.bg(cx.theme().tokens.list_hover))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(repo.display().to_string()),
            )
            .when(current, |this| this.child("当前"))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_dir(path.clone(), window, cx)
            }))
    }

    fn pick_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择仓库目录（不是 Git 仓库会自动初始化）".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let picked = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                // 取消或选空：什么都不做
                Ok(Ok(None)) => None,
                Ok(Err(err)) => {
                    this.update_in(cx, |this, _, cx| {
                        this.report(format!("打开目录选择器失败：{err}"), cx);
                    })
                    .ok();
                    None
                }
                Err(_) => None,
            };
            let Some(repo) = picked else { return };

            this.update_in(cx, |this, window, cx| this.open_dir(repo, window, cx))
                .ok();
        })
        // 任务被 drop 就会取消，所以必须 detach：选择器要继续跑到用户选完。
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return self.report("先选择仓库目录", cx);
        };
        let author = self.author.read(cx).value().trim().to_string();
        let email = self.email.read(cx).value().trim().to_string();
        if let Err(err) = super::validate(&author, &email) {
            return self.report(format!("{err:#}"), cx);
        }

        let config = RepoConfig { author, email };
        match config.save(&repo) {
            Ok(()) => self.report_ok(format!("已保存到 {}/.git/ez-git.toml", repo.display()), cx),
            Err(err) => self.report(format!("{err:#}"), cx),
        }
    }

    fn status_element(&self) -> Option<Alert> {
        match &self.status {
            Status::Idle => None,
            Status::Ok(message) => Some(Alert::success("configuration-status", message.clone())),
            Status::Error(message) => Some(Alert::error("configuration-status", message.clone())),
        }
    }
}

impl Render for ConfigView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let has_repo = self.repo.is_some();
        // 系统标题栏自带关闭按钮；只有客户端自绘装饰时才需要自己画右上角那个
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(client_decorations, |this| {
                this.child(
                    h_flex()
                        .h_9()
                        .px_3()
                        .justify_between()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child("配置")
                        .child(
                            Button::new("close-configuration")
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("关闭")
                                .on_click(|_, window, _| window.remove_window()),
                        ),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .p_4()
                    .gap_4()
                    .text_sm()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_color(muted).child("仓库目录"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(path_box(
                                        self.repo.as_deref(),
                                        "未选择（选中的目录不是 Git 仓库会自动初始化）",
                                        cx,
                                    ))
                                    .child(
                                        Button::new("pick-repository").label("选择目录…").on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.pick_repo(window, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new("reload-configuration")
                                            .label("刷新历史")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.reload(window, cx)
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_color(muted).child(format!(
                                "打开历史（最多 {} 个，最近使用在前；上限在 config.toml 的 history_limit 里改）",
                                self.history_limit
                            )))
                            .child(
                                v_flex()
                                    .id("open-history")
                                    .w_full()
                                    .max_h(px(150.))
                                    .overflow_y_scroll()
                                    .children(
                                        self.history
                                            .iter()
                                            .enumerate()
                                            .map(|(index, repo)| self.history_row(index, repo, cx))
                                            .collect::<Vec<_>>(),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1()
                                    .child(div().text_color(muted).child("作者"))
                                    .child(Input::new(&self.author).disabled(!has_repo)),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1()
                                    .child(div().text_color(muted).child("邮箱"))
                                    .child(Input::new(&self.email).disabled(!has_repo)),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("save-configuration")
                                    .primary()
                                    .disabled(!has_repo)
                                    .label("保存")
                                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                            )
                            .child(div().text_color(muted).child(
                                "默认取 .git/config 的 user.name / user.email；保存只写进该仓库的 .git/ez-git.toml。",
                            )),
                    )
                    .when_some(self.status_element(), |this, alert| this.child(alert)),
            )
    }
}

/// 只读路径展示框，未选择时显示提示文字。
fn path_box(
    path: Option<&std::path::Path>,
    empty_hint: &'static str,
    cx: &App,
) -> impl IntoElement {
    let text = path.map(|path| path.display().to_string());
    div()
        .flex_1()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .overflow_x_hidden()
        .text_color(match text {
            Some(_) => cx.theme().foreground,
            None => cx.theme().muted_foreground,
        })
        .child(text.unwrap_or_else(|| empty_hint.to_string()))
}
