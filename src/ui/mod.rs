mod backend_bridge;
pub(crate) mod connections;
pub(crate) mod native_cards;
pub mod root;
mod settings;
pub mod sidebar;
pub(crate) mod source_labels;
pub(crate) mod source_picker;
mod tabs;
mod title_bar;
pub(crate) mod web_cards;

pub(crate) struct CardVisibility {
    pub active_page: Option<crate::page::PageId>,
    pub editing: std::collections::HashSet<(crate::page::PageId, crate::page::workspace::PaneId)>,
}

impl CardVisibility {
    pub fn is_active(
        &self,
        page: crate::page::PageId,
        card: crate::page::workspace::PaneId,
    ) -> bool {
        self.active_page == Some(page) && !self.editing.contains(&(page, card))
    }
}

#[cfg(test)]
mod tests {
    use super::CardVisibility;

    #[test]
    fn source_editing_pauses_only_its_own_card() {
        let visibility = CardVisibility {
            active_page: Some(1),
            editing: [(1, 2)].into(),
        };
        assert!(!visibility.is_active(1, 2));
        assert!(visibility.is_active(1, 3));
        assert!(!visibility.is_active(4, 3));
    }
}
