use gpui_kit::component::TitleBar;
use gpui_kit::*;

use crate::theme::{ThemeMode, set_theme};
use crate::ui::root::RootView;

actions!(tradeapp, [Quit, LightTheme, DarkTheme]);

pub fn app_run(cx: &mut App) {
    gpui_kit::init(cx);
    set_theme(ThemeMode::Dark, cx);
    // Actions
    set_actions(cx);
    // Keybinds
    set_keybinds(cx);
    // Menu
    set_menus(cx);
    // Windows
    set_windows(cx);
}

fn set_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &LightTheme, cx| set_theme(ThemeMode::Light, cx));
    cx.on_action(|_: &DarkTheme, cx| set_theme(ThemeMode::Dark, cx));
}

fn set_keybinds(cx: &mut App) {
    let quit_shortcut = if cfg!(target_os = "macos") {
        "cmd-q"
    } else {
        "alt-f4"
    };
    cx.bind_keys([KeyBinding::new(quit_shortcut, Quit, None)]);
}

fn set_windows(cx: &mut App) {
    let options = window_options(cx);
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| RootView::new(window, cx))
    })
    .expect("Failed Starting the window!!! Where are you running this???");

    cx.activate(true);
}

fn window_options(cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(800.), px(600.)),
            cx,
        ))),
        window_min_size: Some(size(px(640.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Trade Slope".into()),
            ..TitleBar::title_bar_options()
        }),
        focus: true,
        show: true,
        ..TitleBar::window_options()
    }
}

fn set_menus(cx: &mut App) {
    cx.set_menus([
        Menu::new("Trade Slope").items([MenuItem::action("Quit Trade Slope", Quit)]),
        Menu::new("Appearance").items([
            MenuItem::action("Light", LightTheme),
            MenuItem::action("Dark", DarkTheme),
        ]),
    ]);
}
