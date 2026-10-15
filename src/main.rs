use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

mod branch;
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
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(160.), px(100.)),
                size: size(px(1180.), px(780.)),
            })),
            window_min_size: Some(size(px(840.), px(560.))),
            titlebar: Some(TitlebarOptions {
                title: Some("ez-git".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| workspace::Workspace::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
