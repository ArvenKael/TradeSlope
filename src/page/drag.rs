use super::workspace::{PaneId, SplitDirection};

/// Pointer travel, rather than layout changes under a stationary pointer, drives
/// live docking. This prevents a newly resized neighbor from moving the card back.
#[derive(Default)]
pub struct LiveDrag {
    last: Option<(PaneId, SplitDirection, bool, f32, f32)>,
}

impl LiveDrag {
    pub fn placement(
        &mut self,
        target: PaneId,
        pointer: (f32, f32),
        relative: (f32, f32),
    ) -> Option<(SplitDirection, bool)> {
        let (x, y) = relative;
        let horizontal = x.min(1. - x);
        let vertical = y.min(1. - y);
        let (direction, before) = if horizontal < vertical {
            (SplitDirection::Right, x < 0.5)
        } else {
            (SplitDirection::Below, y < 0.5)
        };
        if let Some((previous, axis, side, last_x, last_y)) = self.last
            && ((previous == target && axis == direction && side == before)
                || (pointer.0 - last_x).hypot(pointer.1 - last_y) < 24.)
        {
            return None;
        }
        self.last = Some((target, direction, before, pointer.0, pointer.1));
        Some((direction, before))
    }
}

#[cfg(test)]
mod tests {
    use super::LiveDrag;
    use crate::page::workspace::SplitDirection;

    #[test]
    fn pointer_location_chooses_nearest_edge() {
        for (relative, expected) in [
            ((0.1, 0.5), (SplitDirection::Right, true)),
            ((0.9, 0.5), (SplitDirection::Right, false)),
            ((0.5, 0.1), (SplitDirection::Below, true)),
            ((0.5, 0.9), (SplitDirection::Below, false)),
        ] {
            assert!(LiveDrag::default().placement(1, (100., 100.), relative) == Some(expected));
        }
    }

    #[test]
    fn reflow_under_stationary_pointer_does_not_redock() {
        let mut drag = LiveDrag::default();
        assert!(drag.placement(1, (100., 100.), (0.1, 0.5)).is_some());
        assert!(drag.placement(2, (100., 100.), (0.9, 0.5)).is_none());
        assert!(drag.placement(2, (110., 110.), (0.9, 0.5)).is_none());
        assert!(drag.placement(2, (140., 100.), (0.9, 0.5)).is_some());
    }

    #[test]
    fn same_placement_does_not_rebuild_split_repeatedly() {
        let mut drag = LiveDrag::default();
        assert!(drag.placement(1, (100., 100.), (0.1, 0.5)).is_some());
        assert!(drag.placement(1, (200., 100.), (0.1, 0.5)).is_none());
    }
}
