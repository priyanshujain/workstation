use crate::key::DisplayKey;

/// A display as macOS currently reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub id: u32,
    pub key: DisplayKey,
    pub builtin: bool,
    pub main: bool,
    pub origin: (i32, i32),
    pub size: (u32, u32),
}

/// Where a display should be placed in the global desktop coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    pub id: u32,
    pub origin: (i32, i32),
}

/// Origins that put `target` at (0,0), which is the position macOS treats as the main
/// display. The whole arrangement is translated by the same offset, so the panels keep
/// their relative positions and only the menu bar moves.
///
/// Returns `None` when the target is not connected, and an empty plan when it is already
/// main.
pub fn plan_moves(displays: &[DisplayInfo], target: DisplayKey) -> Option<Vec<Move>> {
    let anchor = displays.iter().find(|d| d.key == target)?;
    let (dx, dy) = anchor.origin;

    if (dx, dy) == (0, 0) {
        return Some(Vec::new());
    }

    Some(
        displays
            .iter()
            .map(|d| Move {
                id: d.id,
                origin: (d.origin.0 - dx, d.origin.1 - dy),
            })
            .collect(),
    )
}

/// The display holding the menu bar right now.
pub fn current_main(displays: &[DisplayInfo]) -> Option<&DisplayInfo> {
    displays
        .iter()
        .find(|d| d.main)
        .or_else(|| displays.iter().find(|d| d.origin == (0, 0)))
}

/// Panels that share a key cannot be told apart, so pinning either one is a coin flip.
pub fn has_duplicate_key(displays: &[DisplayInfo], key: DisplayKey) -> bool {
    displays.iter().filter(|d| d.key == key).count() > 1
}

/// Where a display sits in the arrangement, as a word rather than a coordinate.
///
/// Identical panels report the same model name, and macOS's own "(1)"/"(2)" suffixes do not
/// follow the arrangement, so neither tells you which physical screen you are looking at.
/// Position does.
pub fn position_label(displays: &[DisplayInfo], id: u32) -> String {
    if displays.len() <= 1 {
        return "only display".to_string();
    }

    let first_x = displays[0].origin.0;
    let horizontal = displays.iter().any(|d| d.origin.0 != first_x);

    let mut order: Vec<&DisplayInfo> = displays.iter().collect();
    if horizontal {
        order.sort_by_key(|d| d.origin.0);
    } else {
        // CGDisplayBounds grows downwards, so the smallest y is the top screen.
        order.sort_by_key(|d| d.origin.1);
    }

    let Some(rank) = order.iter().position(|d| d.id == id) else {
        return "unknown".to_string();
    };

    let (low, high) = if horizontal {
        ("left", "right")
    } else {
        ("top", "bottom")
    };

    match (order.len(), rank) {
        (2, 0) => low.to_string(),
        (2, _) => high.to_string(),
        (_, 0) => format!("{low}most"),
        (n, r) if r == n - 1 => format!("{high}most"),
        (_, r) => format!("#{} from {low}", r + 1),
    }
}

/// Whether the arrangement moved since the last look, and so needs another pass.
/// The first observation always counts as a change so startup enforces once.
pub fn layout_changed(previous: Option<&[DisplayInfo]>, current: &[DisplayInfo]) -> bool {
    previous != Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR_1: DisplayKey = DisplayKey {
        vendor: 4268,
        model: 53466,
        serial: 911690818,
    };
    const MONITOR_2: DisplayKey = DisplayKey {
        vendor: 4268,
        model: 53465,
        serial: 893603394,
    };
    const BUILTIN: DisplayKey = DisplayKey {
        vendor: 1552,
        model: 40967,
        serial: 0,
    };

    fn display(id: u32, key: DisplayKey, origin: (i32, i32), main: bool) -> DisplayInfo {
        DisplayInfo {
            id,
            key,
            builtin: key == BUILTIN,
            main,
            origin,
            size: (1920, 1080),
        }
    }

    #[test]
    fn no_moves_when_target_is_already_main() {
        let displays = vec![
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_2, (1920, 0), false),
        ];
        assert_eq!(plan_moves(&displays, MONITOR_1), Some(Vec::new()));
    }

    #[test]
    fn shifts_arrangement_so_target_lands_at_origin() {
        let displays = vec![
            display(2, MONITOR_1, (1920, 0), false),
            display(3, MONITOR_2, (0, 0), true),
        ];
        let moves = plan_moves(&displays, MONITOR_1).unwrap();
        assert_eq!(
            moves,
            vec![
                Move {
                    id: 2,
                    origin: (0, 0)
                },
                Move {
                    id: 3,
                    origin: (-1920, 0)
                },
            ]
        );
    }

    #[test]
    fn preserves_relative_layout() {
        let displays = vec![
            display(1, BUILTIN, (0, 0), true),
            display(2, MONITOR_1, (1512, -400), false),
            display(3, MONITOR_2, (3432, -400), false),
        ];
        let moves = plan_moves(&displays, MONITOR_1).unwrap();

        let gap_before = displays[2].origin.0 - displays[1].origin.0;
        let gap_after = moves[2].origin.0 - moves[1].origin.0;
        assert_eq!(gap_before, gap_after);

        assert_eq!(moves[1].origin, (0, 0));
        assert_eq!(moves[0].origin, (-1512, 400));
        assert_eq!(moves[2].origin, (1920, 0));
    }

    #[test]
    fn handles_vertical_stacking() {
        let displays = vec![
            display(2, MONITOR_1, (0, 1080), false),
            display(3, MONITOR_2, (0, 0), true),
        ];
        let moves = plan_moves(&displays, MONITOR_1).unwrap();
        assert_eq!(moves[0].origin, (0, 0));
        assert_eq!(moves[1].origin, (0, -1080));
    }

    #[test]
    fn returns_none_when_target_is_disconnected() {
        let displays = vec![display(1, BUILTIN, (0, 0), true)];
        assert_eq!(plan_moves(&displays, MONITOR_1), None);
    }

    #[test]
    fn returns_none_for_empty_display_list() {
        assert_eq!(plan_moves(&[], MONITOR_1), None);
    }

    #[test]
    fn single_display_setup_needs_no_moves() {
        let displays = vec![display(2, MONITOR_1, (0, 0), true)];
        assert_eq!(plan_moves(&displays, MONITOR_1), Some(Vec::new()));
    }

    #[test]
    fn finds_current_main() {
        let displays = vec![
            display(2, MONITOR_1, (1920, 0), false),
            display(3, MONITOR_2, (0, 0), true),
        ];
        assert_eq!(current_main(&displays).unwrap().key, MONITOR_2);
    }

    #[test]
    fn falls_back_to_origin_when_main_flag_is_unset() {
        let displays = vec![
            display(2, MONITOR_1, (1920, 0), false),
            display(3, MONITOR_2, (0, 0), false),
        ];
        assert_eq!(current_main(&displays).unwrap().key, MONITOR_2);
    }

    #[test]
    fn labels_two_side_by_side_panels() {
        // The real desk: identical model names, told apart only by position.
        let displays = vec![
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_2, (1920, 0), false),
        ];
        assert_eq!(position_label(&displays, 2), "left");
        assert_eq!(position_label(&displays, 3), "right");
    }

    #[test]
    fn label_follows_arrangement_not_listing_order() {
        // Same panels, main moved to the right-hand one, so origins go negative.
        let displays = vec![
            display(2, MONITOR_1, (-1920, 0), false),
            display(3, MONITOR_2, (0, 0), true),
        ];
        assert_eq!(position_label(&displays, 2), "left");
        assert_eq!(position_label(&displays, 3), "right");
    }

    #[test]
    fn labels_stacked_panels_by_height() {
        let displays = vec![
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_2, (0, 1080), false),
        ];
        assert_eq!(position_label(&displays, 2), "top");
        assert_eq!(position_label(&displays, 3), "bottom");
    }

    #[test]
    fn labels_three_panels_end_to_end() {
        let displays = vec![
            display(1, BUILTIN, (1920, 0), false),
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_2, (3840, 0), false),
        ];
        assert_eq!(position_label(&displays, 2), "leftmost");
        assert_eq!(position_label(&displays, 1), "#2 from left");
        assert_eq!(position_label(&displays, 3), "rightmost");
    }

    #[test]
    fn single_display_needs_no_disambiguation() {
        let displays = vec![display(2, MONITOR_1, (0, 0), true)];
        assert_eq!(position_label(&displays, 2), "only display");
    }

    #[test]
    fn unknown_id_does_not_panic() {
        let displays = vec![
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_2, (1920, 0), false),
        ];
        assert_eq!(position_label(&displays, 99), "unknown");
    }

    #[test]
    fn first_observation_counts_as_a_change() {
        let displays = vec![display(2, MONITOR_1, (0, 0), true)];
        assert!(layout_changed(None, &displays));
    }

    #[test]
    fn identical_layout_is_not_a_change() {
        let displays = vec![
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_2, (1920, 0), false),
        ];
        assert!(!layout_changed(Some(&displays), &displays));
    }

    #[test]
    fn menu_bar_moving_is_a_change() {
        let before = vec![
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_2, (1920, 0), false),
        ];
        let after = vec![
            display(2, MONITOR_1, (-1920, 0), false),
            display(3, MONITOR_2, (0, 0), true),
        ];
        assert!(layout_changed(Some(&before), &after));
    }

    #[test]
    fn unplugging_a_display_is_a_change() {
        let before = vec![
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_2, (1920, 0), false),
        ];
        let after = vec![display(2, MONITOR_1, (0, 0), true)];
        assert!(layout_changed(Some(&before), &after));
    }

    #[test]
    fn detects_indistinguishable_panels() {
        let displays = vec![
            display(2, MONITOR_1, (0, 0), true),
            display(3, MONITOR_1, (1920, 0), false),
        ];
        assert!(has_duplicate_key(&displays, MONITOR_1));
        assert!(!has_duplicate_key(&displays, MONITOR_2));
    }
}
