use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

mod commit;
mod config;
mod repo;
mod status;
mod sync;
mod workspace;

fn main() {
    application().with_assets(assets::AllAssets).run(|cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| workspace::Workspace::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
