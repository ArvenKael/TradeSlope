use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

pub struct AppAssets;

const ZED_ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/close.svg",
        include_bytes!("../assets/zed-icons/close.svg"),
    ),
    (
        "icons/plus.svg",
        include_bytes!("../assets/zed-icons/plus.svg"),
    ),
    (
        "icons/file-text.svg",
        include_bytes!("../assets/zed-icons/file.svg"),
    ),
    (
        "icons/settings-2.svg",
        include_bytes!("../assets/zed-icons/settings.svg"),
    ),
    (
        "icons/panel-left.svg",
        include_bytes!("../assets/zed-icons/threads_sidebar_left_open.svg"),
    ),
    (
        "icons/zed-sidebar-closed.svg",
        include_bytes!("../assets/zed-icons/threads_sidebar_left_closed.svg"),
    ),
    (
        "licenses/zed-icons.txt",
        include_bytes!("../assets/zed-icons/LICENSES"),
    ),
];

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = ZED_ASSETS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut entries = gpui_kit::assets::Assets.list(path)?;
        entries.extend(
            ZED_ASSETS
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        entries.sort();
        entries.dedup();
        Ok(entries)
    }
}
