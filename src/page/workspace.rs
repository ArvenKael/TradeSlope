use gpui_kit::component::resizable::ResizableState;
use gpui_kit::{App, AppContext, Entity};
use std::collections::HashMap;

use super::CardKind;

pub type PaneId = usize;

pub const MIN_CARD_WIDTH: f32 = 220.;
pub const MIN_CARD_HEIGHT: f32 = 160.;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Right,
    Below,
}

pub enum Pane {
    Card {
        id: PaneId,
        kind: Option<CardKind>,
    },
    Split {
        id: PaneId,
        direction: SplitDirection,
        first: Box<Pane>,
        second: Box<Pane>,
    },
}

impl Pane {
    pub fn minimum_size(&self) -> (f32, f32) {
        match self {
            Self::Card { .. } => (MIN_CARD_WIDTH, MIN_CARD_HEIGHT),
            Self::Split {
                direction,
                first,
                second,
                ..
            } => {
                let (first_width, first_height) = first.minimum_size();
                let (second_width, second_height) = second.minimum_size();
                match direction {
                    SplitDirection::Right => (
                        first_width + second_width + 1.,
                        first_height.max(second_height),
                    ),
                    SplitDirection::Below => (
                        first_width.max(second_width),
                        first_height + second_height + 1.,
                    ),
                }
            }
        }
    }

    fn card_depth(&self, target: PaneId) -> Option<usize> {
        match self {
            Self::Card { id, .. } => (*id == target).then_some(0),
            Self::Split { first, second, .. } => first
                .card_depth(target)
                .or_else(|| second.card_depth(target))
                .map(|depth| depth + 1),
        }
    }

    fn card_mut(&mut self, target: PaneId) -> Option<&mut Self> {
        match self {
            Self::Card { id, .. } if *id == target => Some(self),
            Self::Split { first, second, .. } => {
                first.card_mut(target).or_else(|| second.card_mut(target))
            }
            _ => None,
        }
    }

    fn first_card(&self) -> PaneId {
        match self {
            Self::Card { id, .. } => *id,
            Self::Split { first, .. } => first.first_card(),
        }
    }

    // Collapse the removed card's parent so its sibling reclaims the space.
    fn remove(self, target: PaneId) -> Option<Self> {
        self.detach(target).0
    }

    // Return the original leaf, including its view, without cloning it.
    fn detach(self, target: PaneId) -> (Option<Self>, Option<Self>) {
        match self {
            Self::Card { id, .. } if id == target => (None, Some(self)),
            Self::Split {
                id,
                direction,
                first,
                second,
            } => {
                let (first, first_card) = first.detach(target);
                let (second, second_card) = second.detach(target);
                let remaining = match (first, second) {
                    (Some(first), Some(second)) => Some(Self::Split {
                        id,
                        direction,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (remaining, None) | (None, remaining) => remaining,
                };
                (remaining, first_card.or(second_card))
            }
            card => (Some(card), None),
        }
    }
}

pub struct Workspace {
    pub root: Option<Pane>,
    pub selected: Option<PaneId>,
    // Explicit state keeps divider sizes when the page is not being rendered.
    pub sizes: HashMap<PaneId, Entity<ResizableState>>,
    pub bindings: HashMap<PaneId, tradeslope_sdk::Bindings>,
    next_id: PaneId,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            root: Some(Pane::Card { id: 0, kind: None }),
            selected: Some(0),
            sizes: HashMap::new(),
            bindings: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn select(&mut self, id: PaneId) {
        if self
            .root
            .as_mut()
            .and_then(|root| root.card_mut(id))
            .is_some()
        {
            self.selected = Some(id);
        }
    }

    pub fn choose(&mut self, id: PaneId, kind: Option<CardKind>) {
        if let Some(Pane::Card { kind: current, .. }) =
            self.root.as_mut().and_then(|root| root.card_mut(id))
        {
            if *current != kind {
                self.bindings.remove(&id);
            }
            *current = kind;
            self.selected = Some(id);
        }
    }

    pub fn kind(&self, id: PaneId) -> Option<&CardKind> {
        fn find(pane: &Pane, target: PaneId) -> Option<&CardKind> {
            match pane {
                Pane::Card { id, kind } if *id == target => kind.as_ref(),
                Pane::Split { first, second, .. } => {
                    find(first, target).or_else(|| find(second, target))
                }
                _ => None,
            }
        }
        self.root.as_ref().and_then(|root| find(root, id))
    }

    fn split(&mut self, direction: SplitDirection) -> Option<PaneId> {
        let card_id = self.next_id;
        self.next_id += 1;
        let new_card = Pane::Card {
            id: card_id,
            kind: None,
        };
        if let Some(root) = self.root.as_mut() {
            let target = self.selected.unwrap_or_else(|| root.first_card());
            let card = root.card_mut(target)?;
            let split_id = self.next_id;
            self.next_id += 1;
            let previous = std::mem::replace(card, new_card);
            *card = Pane::Split {
                id: split_id,
                direction,
                first: Box::new(previous),
                second: Box::new(Pane::Card {
                    id: card_id,
                    kind: None,
                }),
            };
            self.selected = Some(card_id);
            Some(split_id)
        } else {
            self.root = Some(new_card);
            self.selected = Some(card_id);
            None
        }
    }

    pub fn add(&mut self, direction: SplitDirection, cx: &mut App) {
        if let Some(id) = self.split(direction) {
            self.sizes.insert(id, cx.new(|_| ResizableState::default()));
        }
    }

    fn auto_direction(&self) -> SplitDirection {
        let depth = self
            .root
            .as_ref()
            .and_then(|root| root.card_depth(self.selected.unwrap_or_else(|| root.first_card())));
        match depth.unwrap_or(0) % 2 {
            0 => SplitDirection::Right,
            _ => SplitDirection::Below,
        }
    }

    pub fn add_auto(&mut self, cx: &mut App) {
        self.add(self.auto_direction(), cx);
    }

    // The tree operation is independent of App so relocation can be tested directly.
    fn relocate(
        &mut self,
        source: PaneId,
        target: PaneId,
        direction: SplitDirection,
        before: bool,
    ) -> Option<PaneId> {
        let root = self.root.as_ref()?;
        if source == target
            || root.card_depth(source).is_none()
            || root.card_depth(target).is_none()
        {
            return None;
        }
        let (remaining, moved) = self.root.take().unwrap().detach(source);
        let mut root = remaining.expect("distinct target remains after detaching source");
        let target = root.card_mut(target).expect("validated target remains");
        let previous = std::mem::replace(
            target,
            Pane::Card {
                id: source,
                kind: None,
            },
        );
        let moved = moved.expect("validated source exists");
        let (first, second) = if before {
            (moved, previous)
        } else {
            (previous, moved)
        };
        let split_id = self.next_id;
        self.next_id += 1;
        *target = Pane::Split {
            id: split_id,
            direction,
            first: Box::new(first),
            second: Box::new(second),
        };
        self.root = Some(root);
        self.selected = Some(source);
        self.prune_sizes();
        Some(split_id)
    }

    pub fn move_card(
        &mut self,
        source: PaneId,
        target: PaneId,
        direction: SplitDirection,
        before: bool,
        cx: &mut App,
    ) {
        if let Some(id) = self.relocate(source, target, direction, before) {
            self.sizes.insert(id, cx.new(|_| ResizableState::default()));
        }
    }

    pub fn remove(&mut self, id: PaneId) {
        self.bindings.remove(&id);
        self.root = self.root.take().and_then(|root| root.remove(id));
        if self.selected == Some(id) {
            self.selected = self.root.as_ref().map(Pane::first_card);
        }
        self.prune_sizes();
    }

    fn prune_sizes(&mut self) {
        fn collect_splits(pane: &Pane, ids: &mut Vec<PaneId>) {
            if let Pane::Split {
                id, first, second, ..
            } = pane
            {
                ids.push(*id);
                collect_splits(first, ids);
                collect_splits(second, ids);
            }
        }
        let mut live = vec![];
        if let Some(root) = &self.root {
            collect_splits(root, &mut live);
        }
        self.sizes.retain(|id, _| live.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_CARD_HEIGHT, MIN_CARD_WIDTH, Pane, SplitDirection, Workspace};
    use crate::page::CardKind;

    #[test]
    fn automatic_direction_tracks_selected_depth_and_empty_workspace() {
        let mut workspace = Workspace::new();
        assert!(workspace.auto_direction() == SplitDirection::Right);
        workspace.split(workspace.auto_direction());
        assert!(workspace.auto_direction() == SplitDirection::Below);
        workspace.split(workspace.auto_direction());
        assert!(workspace.auto_direction() == SplitDirection::Right);
        workspace.select(0);
        assert!(workspace.auto_direction() == SplitDirection::Below);
        workspace.remove(0);
        workspace.remove(1);
        workspace.remove(3);
        assert!(workspace.root.is_none());
        workspace.split(workspace.auto_direction());
        assert!(matches!(workspace.root, Some(Pane::Card { .. })));
    }

    #[test]
    fn nested_moves_preserve_views_and_collapse_old_parent() {
        let mut workspace = Workspace::new();
        workspace.choose(0, Some(CardKind::Native("chart".into())));
        workspace.split(SplitDirection::Right); // split 2: cards 0, 1
        workspace.choose(1, Some(CardKind::JavaScript("orders".into())));
        workspace.split(SplitDirection::Below); // split 4: cards 1, 3
        assert_eq!(
            workspace.relocate(0, 3, SplitDirection::Right, true),
            Some(5)
        );
        assert_eq!(workspace.selected, Some(0));
        let Some(Pane::Split {
            id: 4,
            first,
            second,
            ..
        }) = &workspace.root
        else {
            panic!("old root should collapse to split 4");
        };
        assert!(matches!(
            **first,
            Pane::Card {
                id: 1,
                kind: Some(CardKind::JavaScript(ref kind))
            } if kind == "orders"
        ));
        let Pane::Split {
            id: 5,
            direction,
            first,
            second,
        } = &**second
        else {
            panic!("expected fresh split around target");
        };
        assert!(*direction == SplitDirection::Right);
        assert!(matches!(
            **first,
            Pane::Card {
                id: 0,
                kind: Some(CardKind::Native(ref kind))
            } if kind == "chart"
        ));
        assert!(matches!(**second, Pane::Card { id: 3, kind: None }));

        assert_eq!(
            workspace.relocate(0, 1, SplitDirection::Below, false),
            Some(6)
        );
        let Some(Pane::Split {
            id: 4,
            first,
            second,
            ..
        }) = &workspace.root
        else {
            panic!("unaffected root should retain its id");
        };
        assert!(matches!(**second, Pane::Card { id: 3, .. }));
        let Pane::Split {
            id: 6,
            direction,
            first,
            second,
        } = &**first
        else {
            panic!("expected new split beside card 1");
        };
        assert!(*direction == SplitDirection::Below);
        assert!(matches!(
            **first,
            Pane::Card {
                id: 1,
                kind: Some(CardKind::JavaScript(ref kind))
            } if kind == "orders"
        ));
        assert!(matches!(
            **second,
            Pane::Card {
                id: 0,
                kind: Some(CardKind::Native(ref kind))
            } if kind == "chart"
        ));
        assert_eq!(workspace.selected, Some(0));
    }

    #[test]
    fn invalid_drops_leave_tree_selection_and_id_allocator_unchanged() {
        let mut workspace = Workspace::new();
        workspace.choose(0, Some(CardKind::Native("chart".into())));
        workspace.split(SplitDirection::Right);
        for (source, target) in [(0, 0), (99, 0), (0, 99), (2, 0), (0, 2)] {
            assert_eq!(
                workspace.relocate(source, target, SplitDirection::Below, true),
                None
            );
            assert_eq!(workspace.selected, Some(1));
            assert_eq!(workspace.next_id, 3);
            let Some(Pane::Split {
                id: 2,
                direction,
                first,
                second,
            }) = &workspace.root
            else {
                panic!("invalid drop changed layout");
            };
            assert!(*direction == SplitDirection::Right);
            assert!(matches!(
                **first,
                Pane::Card {
                    id: 0,
                    kind: Some(CardKind::Native(ref kind))
                } if kind == "chart"
            ));
            assert!(matches!(**second, Pane::Card { id: 1, kind: None }));
        }
        workspace.remove(0);
        assert_eq!(workspace.relocate(1, 1, SplitDirection::Right, false), None);
        workspace.remove(1);
        let next_id = workspace.next_id;
        assert_eq!(workspace.relocate(0, 1, SplitDirection::Right, false), None);
        assert_eq!(workspace.next_id, next_id);
        assert!(workspace.root.is_none());
        assert!(workspace.selected.is_none());
    }

    #[test]
    fn minimum_sizes_sum_along_axis_and_max_across_axis() {
        let mut workspace = Workspace::new();
        assert_eq!(
            workspace.root.as_ref().unwrap().minimum_size(),
            (MIN_CARD_WIDTH, MIN_CARD_HEIGHT)
        );
        workspace.split(SplitDirection::Right);
        assert_eq!(
            workspace.root.as_ref().unwrap().minimum_size(),
            (441., 160.)
        );
        workspace.split(SplitDirection::Below);
        assert_eq!(
            workspace.root.as_ref().unwrap().minimum_size(),
            (441., 321.)
        );
        workspace.split(SplitDirection::Below);
        assert_eq!(
            workspace.root.as_ref().unwrap().minimum_size(),
            (441., 482.)
        );
        workspace.select(0);
        workspace.split(SplitDirection::Right);
        assert_eq!(
            workspace.root.as_ref().unwrap().minimum_size(),
            (662., 482.)
        );
    }

    #[test]
    fn split_preserves_view_and_selects_new_card() {
        let mut workspace = Workspace::new();
        workspace.choose(0, Some(CardKind::Native("chart".into())));
        workspace.split(SplitDirection::Right);
        assert_eq!(workspace.selected, Some(1));
        let Some(Pane::Split { first, second, .. }) = workspace.root else {
            panic!("expected split");
        };
        assert!(matches!(
            *first,
            Pane::Card {
                id: 0,
                kind: Some(CardKind::Native(ref kind))
            } if kind == "chart"
        ));
        assert!(matches!(*second, Pane::Card { id: 1, kind: None }));
    }

    #[test]
    fn removing_nested_card_collapses_only_its_parent() {
        let mut workspace = Workspace::new();
        workspace.split(SplitDirection::Right);
        workspace.select(0);
        workspace.split(SplitDirection::Below);
        workspace.remove(0);
        assert_eq!(workspace.selected, Some(3));
        assert!(matches!(workspace.root, Some(Pane::Split { .. })));
        workspace.remove(3);
        assert!(matches!(workspace.root, Some(Pane::Card { id: 1, .. })));
        workspace.remove(1);
        assert!(workspace.root.is_none());
        assert!(workspace.selected.is_none());
        workspace.split(SplitDirection::Right);
        assert!(workspace.root.is_some());
    }

    #[test]
    fn unknown_card_does_not_change_selection_or_layout() {
        let mut workspace = Workspace::new();
        workspace.select(99);
        workspace.choose(99, Some(CardKind::JavaScript("orders".into())));
        workspace.remove(99);
        assert_eq!(workspace.selected, Some(0));
        assert!(matches!(
            workspace.root,
            Some(Pane::Card { id: 0, kind: None })
        ));
    }

    #[test]
    fn mappings_belong_to_blocks_not_pages_or_layout_positions() {
        let mut workspace = Workspace::new();
        workspace.choose(0, Some(CardKind::Native("quote".into())));
        let bindings = tradeslope_sdk::Bindings::from([(
            "input".into(),
            tradeslope_sdk::PortBinding {
                backend: "chosen-source".into(),
                operation: "chosen-output".into(),
                params: serde_json::json!({}),
                fields: Default::default(),
                parameter_fields: Default::default(),
            },
        )]);
        workspace.bindings.insert(0, bindings.clone());
        workspace.split(SplitDirection::Right);
        assert!(
            !workspace.bindings.contains_key(&1),
            "new blocks must start unbound"
        );
        workspace.relocate(0, 1, SplitDirection::Below, false);
        assert_eq!(workspace.bindings.get(&0), Some(&bindings));
        workspace.choose(0, Some(CardKind::Native("quote".into())));
        assert!(
            workspace.bindings.contains_key(&0),
            "choosing the same view preserves its mapping"
        );
        workspace.choose(0, Some(CardKind::JavaScript("other".into())));
        assert!(
            !workspace.bindings.contains_key(&0),
            "a different block contract cannot inherit mappings"
        );
        workspace.bindings.insert(1, bindings);
        workspace.remove(1);
        assert!(!workspace.bindings.contains_key(&1));
    }
}
