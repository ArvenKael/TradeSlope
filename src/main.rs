mod app;
mod assets;
mod extension_manager;
mod extensions;
mod page;
mod theme;
mod ui;

use crate::app::app_run;
use gpui_kit::application;

fn main() {
    application()
        .with_assets(assets::AppAssets)
        .run(|cx| app_run(cx));
}
