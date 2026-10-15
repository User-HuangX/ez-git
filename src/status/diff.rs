//! 右侧文件 diff 面板：完整上下文、增删行高亮和编辑器状态。

use std::path::Path;

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::input::{
    Editor, EditorState, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::file_icons::icon_for;
use super::{DiffKind, DiffLine};

const ADDED_BG: u32 = 0x56d36459;
const DELETED_BG: u32 = 0xf8514959;

/// 右侧面板：选中文件的完整改动，交给 Editor 组件显示。
///
/// 文本是整篇 diff（未改动的原文也在里面），增行和删行分别用绿/红的
/// [`RangeDecoration`] 底色铺出来，行距、选中、搜索、复制都用编辑器的。
pub(crate) struct DiffView {
    state: Entity<EditorState>,
    /// 增删行的标记集合；内容变了就整批换掉
    marks: RangeDecorationCollection,
    path: Option<String>,
    /// 选中的是目录（不显示 diff，只给提示）
    folder: bool,
    error: Option<SharedString>,
    /// 编辑器里现在装的语言；没变就不要再设高亮器（设一次就得重编译 tree-sitter 查询）。
    language: Option<String>,
    /// 待渲染的 diff；换文本需要 window，因此延迟到 render。
    pending: Option<Vec<DiffLine>>,
}

impl DiffView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            // 行号是 diff 文本自己的行号（从 1 数到最后一行），不是文件行号
            EditorState::new(window, cx)
                .line_number(true)
                .default_value("")
        });
        let marks = state.update(cx, |state, cx| {
            state.create_range_decorations_collection(Vec::new(), cx)
        });

        Self {
            state,
            marks,
            path: None,
            folder: false,
            error: None,
            language: None,
            pending: None,
        }
    }

    /// 换文件并带上算好的 diff；目录或没得看就传空。
    pub(crate) fn show(
        &mut self,
        path: Option<String>,
        folder: bool,
        lines: anyhow::Result<Vec<DiffLine>>,
        cx: &mut Context<Self>,
    ) {
        self.path = path;
        self.folder = folder;
        self.error = None;
        // 按文件后缀选语法；注册表自己认别名（rs/md/yml/ts…），不认得的当纯文本
        let language = self
            .path
            .as_deref()
            .and_then(|path| Path::new(path).extension())
            .and_then(|ext| ext.to_str())
            .unwrap_or("text");
        // 语言真的变了才重设：set_highlighter 会丢掉高亮器，下次解析要重编译查询（几十毫秒）
        if self.language.as_deref() != Some(language) {
            self.language = Some(language.to_string());
            self.state
                .update(cx, |state, cx| state.set_highlighter(language, cx));
        }
        self.pending = Some(match lines {
            Ok(lines) => lines,
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                Vec::new()
            }
        });
        cx.notify();
    }
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // set_value 需要 window，所以文本在这里才推进编辑器
        if let Some(lines) = self.pending.take() {
            let started = std::time::Instant::now();
            let (text, marks) = diff_text(&lines);
            let built = started.elapsed();
            // 长度不同就直接换；长度相同（刷新同一个文件）才逐字比，省掉每个文件都整篇比一次
            let state = self.state.read(cx);
            let differs = state.text().len() != text.len() || state.text() != text.as_str();
            // set_value 会把 text 拿走，长度先记下来给下面打印用
            let text_len = text.len();
            let swap = std::time::Instant::now();
            if differs {
                self.state.update(cx, |state, cx| {
                    // 元素把 readonly 写进了 state，换文本那条路会检查 is_editable()：
                    // 先暂时放开、换完再关上（set_value 内部也是这么做的）。
                    state.set_readonly(false, cx);
                    // 走输入事件那条路换文本：set_value 会顺带丢掉高亮器，
                    // 下次解析就得重编译 tree-sitter 查询（release 约 35ms 一次）。
                    let utf16_len: usize = state.text().chars().map(char::len_utf16).sum();
                    state.replace_text_in_range(Some(0..utf16_len), &text, window, cx);
                    // set_value 顺带做的事自己补：清掉选中、滚回顶部
                    state.set_selected_range(0..0, cx);
                    state.set_scroll_offset(point(px(0.), px(0.)), cx);
                    state.set_readonly(true, cx);
                });
            }
            let swapped = swap.elapsed();
            let marks_started = std::time::Instant::now();
            self.marks.set(marks, cx);
            if std::env::var_os("EZ_GIT_TRACE").is_some() {
                eprintln!(
                    "[diff] {} 行 / {} 字节：攒文本 {:?}，换文本 {:?}，装装饰 {:?}",
                    lines.len(),
                    text_len,
                    built,
                    swapped,
                    marks_started.elapsed()
                );
            }
        }

        let empty = self.state.read(cx).text().len() == 0;
        let hint = match self.path.as_deref() {
            None => Some("选择文件，查看更改"),
            Some(_) if self.folder => Some("选择目录中的文件"),
            Some(_) if self.error.is_none() && empty => Some("没有可显示的内容"),
            Some(_) => None,
        };
        let full_path = self.path.clone().unwrap_or_else(|| "Diff 预览".to_string());
        let path = Path::new(&full_path);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&full_path)
            .to_string();
        let parent = path
            .parent()
            .and_then(|parent| parent.to_str())
            .filter(|parent| !parent.is_empty())
            .unwrap_or("工作区")
            .to_string();
        let icon = if self.folder {
            IconName::FolderOpen
        } else if self.path.is_some() {
            icon_for(&full_path)
        } else {
            IconName::FileDiff
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .id("diff-file-header")
                    .h_12()
                    .w_full()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .bg(cx.theme().tokens.group_box)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Icon::new(icon)
                            .size_4()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().text_sm().font_medium().child(name))
                    // 没选文件时不显示无意义的路径占位
                    .when(self.path.is_some(), |header| {
                        header.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(parent),
                        )
                    })
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("只读"),
                    )
                    .tooltip(move |window, cx| Tooltip::new(full_path.clone()).build(window, cx)),
            )
            .when_some(self.error.clone(), |panel, error| {
                panel.child(div().p_4().child(Alert::error("diff-error", error)))
            })
            .when_some(hint, |panel, title| {
                panel.child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .items_center()
                        .justify_center()
                        .p_8()
                        .gap_3()
                        .child(
                            Icon::new(IconName::FileDiff)
                                .size_10()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(div().text_lg().font_medium().child(title))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("完整内容与增删行会显示在这里。"),
                        ),
                )
            })
            .when(hint.is_none(), |panel| {
                panel.child(
                    Editor::new(&self.state)
                        .readonly(true)
                        .appearance(false)
                        .bordered(false)
                        .aria_label("文件内容与 Diff")
                        .flex_1()
                        .min_h_0(),
                )
            })
    }
}

/// 把 diff 行拼成一整篇文本，同时算出增行/删行的字节范围。
///
/// 连续同类行合成一个范围：两万行的大文件不至于堆出两万个装饰。
pub(super) fn diff_text(lines: &[DiffLine]) -> (String, Vec<RangeDecoration>) {
    let mut text = String::new();
    let mut marks = Vec::new();
    // (范围起点, 范围终点, 颜色)，同类行一直往后接
    let mut run: Option<(usize, usize, u32)> = None;

    for line in lines {
        let start = text.len();
        text.push_str(&line.text);
        let end = text.len();
        text.push('\n');

        let color = match line.kind {
            DiffKind::Added => ADDED_BG,
            DiffKind::Deleted => DELETED_BG,
            DiffKind::Header | DiffKind::Context => {
                push_run(&mut marks, run.take());
                continue;
            }
        };
        run = match run {
            Some((run_start, _, run_color)) if run_color == color => Some((run_start, end, color)),
            Some(done) => {
                push_run(&mut marks, Some(done));
                Some((start, end, color))
            }
            None => Some((start, end, color)),
        };
    }
    push_run(&mut marks, run.take());
    (text, marks)
}

/// 把攒着的一串同类行收成一个填充范围。
fn push_run(marks: &mut Vec<RangeDecoration>, run: Option<(usize, usize, u32)>) {
    if let Some((start, end, color)) = run {
        marks.push(
            RangeDecoration::new(start..end)
                .with_style(RangeDecorationStyle::Fill)
                .with_color(rgba(color).into()),
        );
    }
}
