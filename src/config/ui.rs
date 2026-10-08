//! 左上角的仓库入口；打开历史、目录选择和身份表单由 window 模块负责。

use gpui_kit::component::button::{Button, ButtonVariants};

/// 左上角的仓库按钮：文字是当前仓库，点开就是配置。
pub(crate) fn repo_button(label: String) -> Button {
    Button::new("repository")
        .ghost()
        .label(label)
        .accessibility_label("选择仓库")
        .on_click(|_, _, cx| super::window::open(cx))
}
