//! 专用 Diff 组件负责行号、语法高亮、选择复制和增删行展示。
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::diff::{
    Diff, DiffFile, DiffLinePosition, DiffLineRange, DiffMode, DiffSide, DiffState,
};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{DiffKind, DiffLine};

pub(crate) struct DiffView {
    state: Entity<DiffState>,
    path: Option<String>,
    folder: bool,
    empty: bool,
    error: Option<SharedString>,
    search: Entity<InputState>,
    searchable: Vec<(DiffSide, usize, String)>,
    search_cursor: usize,
    search_result: String,
}

impl DiffView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索文件内容"));
        let state = cx.new(|cx| {
            DiffState::new([], cx)
                .with_mode(DiffMode::Unified)
                .with_context_lines(None)
        });
        Self {
            state,
            path: None,
            folder: false,
            empty: true,
            error: None,
            search,
            searchable: Vec::new(),
            search_cursor: 0,
            search_result: String::new(),
        }
    }

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
        self.search_cursor = 0;
        self.search_result.clear();
        self.searchable.clear();
        let files = lines.and_then(|lines| {
            self.searchable = searchable_lines(&lines);
            files_from_lines(self.path.as_deref().unwrap_or("file"), &lines)
        });
        let files = match files {
            Ok(files) => files,
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                Vec::new()
            }
        };
        self.empty = files.is_empty();
        self.state
            .update(cx, |state, cx| state.set_files(files, cx));
        cx.notify();
    }
}

impl DiffView {
    fn find_next(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_lowercase();
        if query.is_empty() {
            return;
        }
        let matches: Vec<_> = self
            .searchable
            .iter()
            .filter(|(_, _, text)| text.to_lowercase().contains(&query))
            .collect();
        if matches.is_empty() {
            self.search_result = "无匹配".into();
        } else if let Some(path) = self
            .state
            .read(cx)
            .files()
            .first()
            .map(|f| f.path().clone())
        {
            let index = self.search_cursor % matches.len();
            let (side, line, _) = matches[index];
            self.state.update(cx, |state, cx| {
                state.set_selected_lines(
                    Some(DiffLineRange::new(path.clone(), *side, *line, *line)),
                    cx,
                );
                state.scroll_to_line(DiffLinePosition::new(path, *side, *line), cx);
            });
            self.search_result = format!("{} / {}", index + 1, matches.len());
            self.search_cursor = index + 1;
        }
        cx.notify();
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hint = if self.path.is_none() {
            Some("选择文件，查看更改")
        } else if self.folder {
            Some("选择目录中的文件")
        } else if self.empty {
            Some("没有可显示的内容")
        } else {
            None
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .h_12()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .bg(cx.theme().tokens.group_box)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(IconName::FileDiff).size_4())
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .child(self.path.clone().unwrap_or_else(|| "Diff 预览".into())),
                    ),
            )
            .when_some(self.error.clone(), |panel, error| {
                panel.child(Alert::error("diff-error", error))
            })
            .when_some(hint, |panel, hint| {
                panel.child(
                    div()
                        .p_8()
                        .text_color(cx.theme().muted_foreground)
                        .child(hint),
                )
            })
            .when(hint.is_none(), |panel| {
                panel
                    .child(
                        h_flex()
                            .px_4()
                            .py_2()
                            .gap_2()
                            .items_center()
                            .child(
                                Input::new(&self.search)
                                    .aria_label("搜索 Diff 内容")
                                    .flex_1(),
                            )
                            .child(
                                Button::new("find-next")
                                    .ghost()
                                    .label("查找下一个")
                                    .on_click(cx.listener(|this, _, _, cx| this.find_next(cx))),
                            )
                            .child(div().text_xs().child(self.search_result.clone())),
                    )
                    .child(
                        Diff::new(&self.state)
                            .header_visible(false)
                            .line_number(true)
                            .syntax_highlight(true)
                            .change_background(true)
                            .flex_1()
                            .min_h_0(),
                    )
            })
    }
}

/// 搜索使用真实文件行号，而不是含文件头的 patch 行号。
fn searchable_lines(lines: &[DiffLine]) -> Vec<(DiffSide, usize, String)> {
    let mut result = Vec::new();
    let (mut old, mut new) = (1, 1);
    for line in lines {
        if line.kind == DiffKind::Header {
            for header in line.text.lines().filter(|s| s.starts_with("@@ ")) {
                let mut fields = header.split_whitespace().skip(1);
                let start = |field: &str| {
                    field
                        .get(1..)
                        .and_then(|s| s.split(',').next())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1)
                };
                if let (Some(a), Some(b)) = (fields.next(), fields.next()) {
                    old = start(a);
                    new = start(b);
                }
            }
            continue;
        }
        if line.text.starts_with('\\') {
            continue;
        }
        match line.kind {
            DiffKind::Deleted => {
                result.push((DiffSide::Original, old, line.text.clone()));
                old += 1;
            }
            DiffKind::Added => {
                result.push((DiffSide::Modified, new, line.text.clone()));
                new += 1;
            }
            DiffKind::Context => {
                result.push((DiffSide::Modified, new, line.text.clone()));
                old += 1;
                new += 1;
            }
            DiffKind::Header => {}
        }
    }
    result
}

/// libgit2 的 patch 行已带换行；工作区原文行则没有，不能无条件添加空行。
fn files_from_lines(path: &str, lines: &[DiffLine]) -> anyhow::Result<Vec<DiffFile>> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let mut patch = String::new();
    if !lines.iter().any(|line| line.kind == DiffKind::Header) {
        // unchanged 避免把文件名中的空格、引号解释成 patch 语法。
        let text = lines
            .iter()
            .map(|line| line.text.strip_prefix(' ').unwrap_or(&line.text))
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(vec![DiffFile::unchanged(path.to_owned(), &text)]);
    }
    for line in lines {
        patch.push_str(&line.text);
        if !line.text.ends_with('\n') {
            patch.push('\n');
        }
    }
    Ok(DiffFile::parse(&patch)?)
}

#[cfg(test)]
mod tests {
    use super::{DiffKind, DiffLine, DiffSide, files_from_lines, searchable_lines};

    #[test]
    fn parses_patch_without_extra_blank_lines() {
        let lines = vec![
            DiffLine {
                kind: DiffKind::Header,
                text: "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n".into(),
            },
            DiffLine {
                kind: DiffKind::Deleted,
                text: "-old\n".into(),
            },
            DiffLine {
                kind: DiffKind::Added,
                text: "+new\n".into(),
            },
        ];
        let files = files_from_lines("a.rs", &lines).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].additions(), 1);
        assert_eq!(files[0].deletions(), 1);
    }

    #[test]
    fn search_uses_hunk_line_numbers_and_sides() {
        let lines = vec![
            DiffLine {
                kind: DiffKind::Header,
                text: "@@ -10,2 +20,2 @@\n".into(),
            },
            DiffLine {
                kind: DiffKind::Context,
                text: " context\n".into(),
            },
            DiffLine {
                kind: DiffKind::Deleted,
                text: "-old\n".into(),
            },
            DiffLine {
                kind: DiffKind::Added,
                text: "+new\n".into(),
            },
        ];
        let positions = searchable_lines(&lines);
        assert_eq!((positions[0].0, positions[0].1), (DiffSide::Modified, 20));
        assert_eq!((positions[1].0, positions[1].1), (DiffSide::Original, 11));
        assert_eq!((positions[2].0, positions[2].1), (DiffSide::Modified, 21));
    }

    #[test]
    fn parses_add_delete_binary_and_missing_final_newline() {
        for patch in [
            "diff --git a/new b/new\nnew file mode 100644\n--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+hello\n",
            "diff --git a/old b/old\ndeleted file mode 100644\n--- a/old\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n",
            "diff --git a/a b/a\nBinary files a/a and b/a differ\n",
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n",
        ] {
            let files = files_from_lines(
                "a",
                &[DiffLine {
                    kind: DiffKind::Header,
                    text: patch.into(),
                }],
            )
            .unwrap();
            assert_eq!(files.len(), 1);
        }
    }

    #[test]
    fn supports_unchanged_and_empty_files() {
        assert!(files_from_lines("empty", &[]).unwrap().is_empty());
        let files = files_from_lines(
            "有 空格.rs",
            &[DiffLine {
                kind: DiffKind::Context,
                text: " hello".into(),
            }],
        )
        .unwrap();
        assert_eq!(files[0].path().as_ref(), "有 空格.rs");
        assert!(!files[0].has_changes());
    }
}
