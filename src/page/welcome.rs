use gpui_kit::*;

pub fn welcome_page() -> impl IntoElement {
    div()
        .size_full()
        .p_4()
        .child("Open a page from the sidebar, or create one with +.")
}
