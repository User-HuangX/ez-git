//! 右侧文件 diff 面板：完整上下文、增删行高亮和编辑器状态。

use gpui_kit::component::input::{
    Editor, EditorState, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle,
};
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{DiffKind, DiffLine};

const RED: u32 = 0xf87171;
const MUTED: u32 = 0x8888aa;
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
    /// 待渲染的 diff；set_value 需要 window，因此延迟到 render。
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
            let (text, marks) = diff_text(&lines);
            if self.state.read(cx).text() != text.as_str() {
                self.state
                    .update(cx, |state, cx| state.set_value(text, window, cx));
            }
            self.marks.set(marks, cx);
        }

        let empty = self.state.read(cx).text() == "";
        let hint = match self.path.as_deref() {
            None => Some("从左树选一个文件看改动"),
            Some(_) if self.folder => Some("这是目录，点里面的文件看改动"),
            Some(_) if self.error.is_none() && empty => Some("这个文件没有改动"),
            Some(_) => None,
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .when_some(self.path.clone(), |this, path| {
                this.child(
                    div()
                        .w_full()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(path),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(RED))
                        .child(error),
                )
            })
            .when_some(hint, |this, hint| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(hint),
                )
            })
            .when(hint.is_none(), |this| {
                this.child(Editor::new(&self.state).readonly(true).flex_1().min_h_0())
            })
    }
}

/// 把 diff 行拼成一整篇文本，同时算出增行/删行的字节范围。
fn diff_text(lines: &[DiffLine]) -> (String, Vec<RangeDecoration>) {
    let mut text = String::new();
    let mut marks = Vec::new();

    for line in lines {
        let start = text.len();
        text.push_str(&line.text);
        let end = text.len();
        text.push('\n');

        let color = match line.kind {
            DiffKind::Added => ADDED_BG,
            DiffKind::Deleted => DELETED_BG,
            DiffKind::Header | DiffKind::Context => continue,
        };
        marks.push(
            RangeDecoration::new(start..end)
                .with_style(RangeDecorationStyle::Fill)
                .with_color(rgba(color).into()),
        );
    }
    (text, marks)
}
