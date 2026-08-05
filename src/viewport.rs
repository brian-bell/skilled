//! Responsive viewport classes.
//!
//! The prototype collapses its two-column workspace into a single column below
//! a breakpoint. Terminals are measured in cells rather than pixels, so the
//! breakpoint is expressed as a column count and every screen asks for a class
//! instead of testing raw widths.

use ratatui::layout::{Constraint, Layout, Rect};

/// The narrowest workspace that can hold a primary region and a detail region
/// side by side without either becoming unreadable.
pub(crate) const WIDE_MINIMUM_WIDTH: u16 = 100;

/// The detail region's share of a wide workspace.
pub(crate) const DETAIL_REGION_WIDTH: u16 = 40;

/// The workspace width from which the detail region takes its full share.
///
/// The threshold is where the inventory table's capped identity columns have
/// nothing left to gain: at 151 columns the primary region keeps 101 even
/// beside the wide aside, both caps bind, and every table column is identical
/// on either side of the crossing. Any lower threshold would hand the aside
/// columns the table was still using, so widening the terminal past it would
/// ellipsize names that fit just before — the opposite of what widening
/// should do.
pub(crate) const DETAIL_REGION_WIDE_THRESHOLD: u16 = 151;

/// The detail region's share of a workspace wide enough to spare it, matching
/// the prototype's fixed 400px aside at roughly eight pixels a cell.
///
/// Below the threshold the aside is narrowed instead of the table: the columns
/// beside it are the reason the workspace is split at all.
pub(crate) const DETAIL_REGION_WIDTH_WIDE: u16 = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Viewport {
    /// One region at a time.
    Compact,
    /// Primary and detail regions together.
    Wide,
}

pub(crate) fn classify(area: Rect) -> Viewport {
    if area.width >= WIDE_MINIMUM_WIDTH {
        Viewport::Wide
    } else {
        Viewport::Compact
    }
}

/// How many columns the detail region takes from a workspace of this width.
pub(crate) fn detail_region_width(workspace_width: u16) -> u16 {
    if workspace_width >= DETAIL_REGION_WIDE_THRESHOLD {
        DETAIL_REGION_WIDTH_WIDE
    } else {
        DETAIL_REGION_WIDTH
    }
}

/// Split a workspace into its primary region and, when the viewport allows it,
/// a detail region.
pub(crate) fn workspace_regions(area: Rect) -> (Rect, Option<Rect>) {
    match classify(area) {
        Viewport::Compact => (area, None),
        Viewport::Wide => {
            let [primary, detail] = Layout::horizontal([
                Constraint::Min(1),
                Constraint::Length(detail_region_width(area.width)),
            ])
            .areas(area);
            (primary, Some(detail))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16) -> Rect {
        Rect::new(0, 2, width, 20)
    }

    #[test]
    fn the_supported_minimum_terminal_is_compact() {
        assert_eq!(classify(area(80)), Viewport::Compact);
        assert_eq!(classify(area(99)), Viewport::Compact);
    }

    #[test]
    fn a_hundred_columns_or_more_is_wide() {
        assert_eq!(classify(area(100)), Viewport::Wide);
        assert_eq!(classify(area(120)), Viewport::Wide);
    }

    #[test]
    fn a_compact_workspace_has_no_detail_region() {
        let (primary, detail) = workspace_regions(area(80));

        assert_eq!(primary, area(80));
        assert_eq!(detail, None);
    }

    #[test]
    fn wide_regions_tile_the_workspace_without_overlap_or_gaps() {
        let workspace = area(120);
        let (primary, detail) = workspace_regions(workspace);
        let detail = detail.expect("wide workspace has a detail region");

        assert_eq!(primary.x, workspace.x);
        assert_eq!(primary.width + detail.width, workspace.width);
        assert_eq!(detail.x, primary.x + primary.width);
        assert_eq!(detail.width, DETAIL_REGION_WIDTH);
        assert_eq!(primary.height, workspace.height);
        assert_eq!(detail.height, workspace.height);
    }

    #[test]
    fn a_workspace_below_the_wide_detail_threshold_keeps_the_narrow_detail() {
        assert_eq!(detail_region_width(100), DETAIL_REGION_WIDTH);
        assert_eq!(detail_region_width(150), DETAIL_REGION_WIDTH);
    }

    #[test]
    fn a_workspace_at_or_above_the_threshold_widens_the_detail() {
        assert_eq!(detail_region_width(151), DETAIL_REGION_WIDTH_WIDE);
        assert_eq!(detail_region_width(180), DETAIL_REGION_WIDTH_WIDE);
    }

    #[test]
    fn the_widened_detail_still_tiles_the_workspace() {
        let workspace = area(180);
        let (primary, detail) = workspace_regions(workspace);
        let detail = detail.expect("wide workspace has a detail region");

        assert_eq!(detail.width, DETAIL_REGION_WIDTH_WIDE);
        assert_eq!(primary.width + detail.width, workspace.width);
        assert_eq!(detail.x, primary.x + primary.width);
    }
}
