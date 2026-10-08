//! 顶栏推送按钮：后台执行远程推送，并展示结果通知。

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

/// 工具栏左侧的推送图标入口。
pub(crate) fn push_button() -> Button {
    Button::new("push")
        .ghost()
        .icon(IconName::CloudUpload)
        .accessibility_label("推送")
        .on_click(|_, window, cx| push(window, cx))
}

fn push(window: &mut Window, cx: &mut App) {
    // 推送走网络，放后台线程，别卡住界面
    let task = cx.background_executor().spawn(async { super::push() });
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            cx.update(|window, cx| match result {
                Ok(()) => window.push_notification(Notification::success("已推送"), cx),
                Err(err) => {
                    window.push_notification(Notification::error(format!("推送失败：{err:#}")), cx)
                }
            })
            .ok();
        })
        // 任务被 drop 就会取消，所以 detach
        .detach();
}
