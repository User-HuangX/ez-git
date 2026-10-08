//! 顶栏的提交按钮，以及提交说明弹窗。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::status::ui::StatusTreeHandle;

/// 工具栏左侧的提交图标入口。
pub(crate) fn commit_button() -> Button {
    Button::new("commit")
        .ghost()
        .icon(IconName::GitCommitHorizontal)
        .accessibility_label("提交")
        .on_click(|_, window, cx| open_message_dialog(window, cx))
}

/// 提交说明弹窗：填完点「提交」才真正提交。
fn open_message_dialog(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| CommitDialog::new(window, cx));

    window.open_dialog(cx, move |dialog, _, _| {
        let confirm = view.clone();
        dialog
            .title("提交")
            .w(px(560.))
            .child(view.clone())
            // 普通 Dialog 不会自己画按钮，footer 里自己放
            .footer(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("commit-cancel")
                            .label("取消")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("commit-confirm")
                            .primary()
                            .label("提交")
                            .on_click(move |_, window, cx| {
                                // 提交失败就保持弹窗开着，方便改完再提交
                                if confirm.update(cx, |dialog, cx| dialog.commit(window, cx)) {
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
            )
    });
}

struct CommitDialog {
    message: Entity<TextareaState>,
    error: Option<SharedString>,
}

impl CommitDialog {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            message: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(4)
                    .placeholder("提交说明，第一行是标题")
            }),
            error: None,
        }
    }

    /// 返回 true 表示提交成功、可以关弹窗。
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let message = self.message.read(cx).value().to_string();
        match super::commit_all(&message) {
            Ok(oid) => {
                self.error = None;
                // 提交后状态树和 diff 要重新读一遍
                let handle = cx.try_global::<StatusTreeHandle>().cloned();
                if let Some(handle) = handle {
                    handle.refresh(cx);
                }
                let short = &oid.to_string()[..8];
                window.push_notification(Notification::success(format!("已提交 {short}")), cx);
                true
            }
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                cx.notify();
                false
            }
        }
    }
}

impl Render for CommitDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .text_sm()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("提交说明"),
            )
            .child(Textarea::new(&self.message))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("工作区所有改动会一起暂存并提交。"),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_color(cx.theme().danger).child(error))
            })
    }
}
