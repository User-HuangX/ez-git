//! 左上角的仓库入口；打开历史、目录选择和身份表单由 window 模块负责。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::*;

/// 左上角的仓库按钮：文字是当前仓库，点开就是配置。
pub(crate) fn repo_button(label: String) -> Button {
    Button::new("repository")
        .ghost()
        .icon(IconName::FolderGit2)
        .label(label)
        .max_w(px(240.))
        .overflow_hidden()
        .accessibility_label("打开仓库与身份配置")
        .on_click(|_, _, cx| super::window::open(cx))
}
