//! Shared presentation primitives translated from `spec/tui-prototype.html`.
//!
//! Components are pure: they build [`Line`]s and [`Span`]s from data the caller
//! already holds. Nothing here reads the filesystem, the database, or the
//! terminal.

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
};

use crate::theme::{self, Tone};

/// One advertised command in the persistent key-hint bar.
///
/// A hint may only be constructed for a command the active context actually
/// handles; the bar is a contract with the user, not decoration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KeyHint {
    key: &'static str,
    label: &'static str,
    essential: bool,
}

impl KeyHint {
    /// A hint that may be dropped when the row runs out of room.
    pub(crate) const fn new(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            essential: false,
        }
    }

    /// A hint that survives truncation.
    ///
    /// Reserved for the commands a user needs in order to leave or complete
    /// the current context. Losing the way out of a dialog to a narrow
    /// terminal would be worse than losing any other hint.
    pub(crate) const fn essential(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            essential: true,
        }
    }

    fn width(self) -> usize {
        cell_width(self.key) + 1 + cell_width(self.label)
    }
}

/// Widths are measured in terminal cells throughout, never bytes or scalars.
///
/// Source labels and skill directory names come from the filesystem, so they
/// can contain full-width or combining characters that occupy a number of
/// cells unrelated to their character count. Ratatui lays text out by cell, so
/// anything that computes padding or a budget must agree with it.
fn cell_width(value: &str) -> usize {
    Span::raw(value).width()
}

const HINT_SEPARATOR: &str = "   ";
const OVERFLOW_MARK: &str = " …";

/// Lay out as many complete hints as fit.
///
/// Hints are dropped whole: a clipped hint would advertise a command the user
/// cannot read, so the row ends with an overflow mark instead. Essential hints
/// are kept in preference to any others, and the surviving hints stay in their
/// declared order so the row does not reshuffle as the terminal resizes.
pub(crate) fn key_hint_line(hints: &[KeyHint], width: u16) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }

    let kept = fitting_hints(hints, width);
    let dropped = kept.len() < hints.len();

    let mut spans = vec![Span::styled(" ", theme::chrome())];
    let mut used = 1;
    for (position, index) in kept.iter().enumerate() {
        if position > 0 {
            spans.push(Span::styled(HINT_SEPARATOR, theme::chrome()));
        }
        spans.push(Span::styled(hints[*index].key, theme::key_cap()));
        spans.push(Span::styled(" ", theme::chrome()));
        spans.push(Span::styled(hints[*index].label, theme::key_label()));
        used += hints[*index].width()
            + if position > 0 {
                cell_width(HINT_SEPARATOR)
            } else {
                0
            };
    }
    // A row too narrow even for the mark says nothing rather than overflowing.
    if dropped && used + cell_width(OVERFLOW_MARK) <= usize::from(width) {
        spans.push(Span::styled(OVERFLOW_MARK, theme::key_label()));
    }
    Line::from(spans)
}

/// Choose the indices that fit, preferring essential hints.
fn fitting_hints(hints: &[KeyHint], width: u16) -> Vec<usize> {
    let width = usize::from(width);
    // One leading column, plus room for the mark once anything is dropped.
    let laid_out = |kept: &[usize], truncated: bool| {
        let content: usize = kept
            .iter()
            .enumerate()
            .map(|(position, index)| {
                hints[*index].width()
                    + if position == 0 {
                        0
                    } else {
                        cell_width(HINT_SEPARATOR)
                    }
            })
            .sum();
        1 + content
            + if truncated {
                cell_width(OVERFLOW_MARK)
            } else {
                0
            }
    };

    let everything: Vec<usize> = (0..hints.len()).collect();
    if laid_out(&everything, false) <= width {
        return everything;
    }

    // Essentials first, then ordinary hints in order while the row still fits.
    let mut kept: Vec<usize> = hints
        .iter()
        .enumerate()
        .filter(|(_, hint)| hint.essential)
        .map(|(index, _)| index)
        .collect();
    let essentials = kept.len();
    // Essentials are declared in the order they are read, and the way out is
    // declared last, so trim from the front to keep it.
    while !kept.is_empty() && laid_out(&kept, true) > width {
        kept.remove(0);
    }
    if kept.len() < essentials {
        // The row cannot even hold the commands the user needs to leave. Show
        // as many of those as fit rather than filling the gap with hints that
        // matter less.
        return kept;
    }
    // Ordinary hints fill from the left and stop at the first that does not
    // fit. Skipping one to squeeze in a later hint would leave a gap the user
    // cannot see, since the overflow mark only appears at the end of the row.
    for (index, hint) in hints.iter().enumerate() {
        if hint.essential {
            continue;
        }
        let mut candidate = kept.clone();
        candidate.push(index);
        candidate.sort_unstable();
        if laid_out(&candidate, true) > width {
            break;
        }
        kept = candidate;
    }
    kept
}

/// The glyph that carries a tone's meaning without colour.
///
/// These are single scalars, but several are East-Asian-ambiguous width and so
/// occupy two cells in a terminal configured for CJK. Alignment is therefore
/// best-effort; the label beside the glyph, not the column, carries the
/// meaning.
pub(crate) fn tone_glyph(tone: Tone) -> &'static str {
    match tone {
        Tone::Healthy => "✓",
        Tone::Warning => "!",
        Tone::Critical => "×",
        Tone::Unmanaged => "U",
        Tone::Inactive => "-",
    }
}

/// A status badge: glyph, then label, both in the tone's colour.
///
/// The glyph and the label each carry the meaning on their own, so the badge
/// still reads on a monochrome terminal or to a user who cannot distinguish
/// the palette.
pub(crate) fn badge(tone: Tone, label: &str) -> Span<'static> {
    Span::styled(
        format!("{} {label}", tone_glyph(tone)),
        theme::tone_style(tone),
    )
}

/// The shared frame for every modal dialog.
///
/// The prototype's dialog anatomy is a header naming the dialog, a scope note,
/// and a body. The border and the header do the work of signalling modality so
/// the dialog does not depend on a tinted backdrop to read as blocking.
pub(crate) fn dialog_frame(title: &str, scope: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dialog_border())
        .title_top(Line::from(vec![
            Span::raw(" "),
            Span::styled(title.to_owned(), theme::dialog_title()),
            Span::raw(" "),
        ]))
        .title_top(
            Line::from(Span::styled(format!(" {scope} "), theme::dialog_scope())).right_aligned(),
        )
        .style(theme::dialog_surface())
        .padding(Padding::new(2, 2, 1, 1))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DialogRegions {
    pub(crate) body: Rect,
    pub(crate) divider: Rect,
    pub(crate) status: Rect,
    pub(crate) actions: Rect,
}

/// Divide a dialog interior into a wrapped body and an explicit footer.
pub(crate) fn dialog_regions(inner: Rect, action_width: u16) -> DialogRegions {
    let [body, divider, footer] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let [status, actions] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(action_width.min(footer.width)),
    ])
    .areas(footer);

    DialogRegions {
        body,
        divider,
        status,
        actions,
    }
}

/// The one text row of a band drawn taller than its content: pad–text–pad.
///
/// The prototype centres a chrome bar's single line vertically
/// (`.terminal-titlebar` is a flex row with `align-items: center`), and whole
/// rows of padding either side are how a terminal says that. The caller
/// paints the band across the full rectangle and renders its text in the row
/// this returns; integer halving keeps the middle row for any odd height and
/// the lower of the two middle rows for an even one, where true centring
/// does not exist.
pub(crate) fn centered_band_row(band: Rect) -> Rect {
    Rect {
        y: band.y + band.height / 2,
        height: band.height.min(1),
        ..band
    }
}

/// Text from the filesystem, made safe to write to a terminal.
///
/// Skill names, catalog paths, checkout directories, link targets, and the text
/// of an operating-system error all come from outside Skilled, and a control
/// sequence in any of them would be executed by the terminal rather than shown.
/// Every surface that puts one in front of a reader passes it through here —
/// the screens, and the `skilled install` command, which writes to the same
/// terminal by a different route.
pub(crate) fn terminal_safe(value: &str) -> String {
    let mut safe = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            safe.extend(character.escape_default());
        } else {
            safe.push(character);
        }
    }
    safe
}

/// The marker that identifies the focused row in a list.
pub(crate) const FOCUS_MARKER: &str = "▌";

/// A selectable row in a list.
///
/// The prototype signals selection with a tinted background and an inset
/// accent bar. A terminal cannot rely on either alone, so focus is carried by a
/// leading marker and bold text as well.
pub(crate) fn list_row(content: Vec<Span<'static>>, selected: bool, width: u16) -> Line<'static> {
    list_row_lines(vec![content], selected, width)
        .pop()
        .expect("one line in, one line out")
}

/// A selectable entry that occupies several lines.
///
/// The prototype draws its multi-line rows with an inset accent bar running
/// the full height of the selected entry. A terminal has one column to spend
/// on that, so the marker repeats down every line of the entry rather than
/// sitting only beside its first: a marker on the first line alone would say
/// where the entry starts, not how far it reaches. The band pads each line to
/// the region width for the same reason it does on a flat row, and unselected
/// entries keep the marker column as indent so the lines stay in one column.
pub(crate) fn list_row_lines(
    lines: Vec<Vec<Span<'static>>>,
    selected: bool,
    width: u16,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|content| {
            let mut spans = vec![
                Span::styled(
                    if selected { FOCUS_MARKER } else { " " },
                    theme::focus_marker(),
                ),
                Span::raw(" "),
            ];
            spans.extend(content);

            if !selected {
                return Line::from(spans);
            }

            // Pad to the region width so the tint reads as a band across the
            // row rather than stopping at the end of the label.
            let line = Line::from(spans);
            let padding = usize::from(width).saturating_sub(line.width());
            let mut spans = line.spans;
            if padding > 0 {
                spans.push(Span::raw(" ".repeat(padding)));
            }
            Line::from(spans).style(theme::selected_row())
        })
        .collect()
}

/// A pane heading with a subtitle that quantifies what the pane contains, and
/// whose active state remains visible without colour.
///
/// The marker column stays as indent when the pane is not focused, the same
/// way [`list_row_lines`] keeps it for unselected entries: the heading holds
/// one column as focus moves, and an unfocused pane keeps its left padding
/// rather than sitting flush against the pane edge.
pub(crate) fn focused_pane_header(heading: &str, subtitle: &str, focused: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if focused { FOCUS_MARKER } else { " " },
            theme::focus_marker(),
        ),
        Span::raw(" "),
        Span::styled(heading.to_owned(), theme::pane_heading()),
        Span::styled(format!("  {subtitle}"), theme::pane_subtitle()),
    ])
}

/// A horizontal rule spanning a region.
pub(crate) fn rule(width: u16) -> Line<'static> {
    Line::from(Span::styled("─".repeat(usize::from(width)), theme::rule()))
}

/// A horizontal rule drawn along the bottom edge of its row, the way the
/// navigation strip draws its border.
///
/// `─` ink sits at the vertical centre of the cell, half a row lower than
/// the border it stands for; where a border closes a block whose clearance
/// must read symmetric — a padded pane header — the eighth-block keeps the
/// ink a whole row beneath the content, matching the blank row above it.
pub(crate) fn underline(width: u16) -> Line<'static> {
    Line::from(Span::styled("▁".repeat(usize::from(width)), theme::rule()))
}

/// A terminal-readable segmented progress line.
///
/// Each segment carries a glyph as well as a colour: completed steps use a
/// check, the active step uses a filled circle, and pending steps use an open
/// circle. The caller supplies a one-based current step.
pub(crate) fn segmented_progress(current: usize, total: usize, width: u16) -> Line<'static> {
    if total == 0 || width == 0 {
        return Line::default();
    }

    let available = usize::from(width).saturating_sub(total.saturating_sub(1));
    let base_width = available / total;
    let wider_segments = available % total;
    let mut spans = Vec::with_capacity(total.saturating_mul(2));

    for index in 1..=total {
        if index > 1 {
            spans.push(Span::raw(" "));
        }
        let segment_width = base_width + usize::from(index <= wider_segments);
        let (glyph, bar, style) = if index < current {
            ('✓', '━', theme::progress_complete())
        } else if index == current {
            ('●', '━', theme::progress_active())
        } else {
            ('○', '─', theme::progress_pending())
        };
        let mut segment = String::new();
        if segment_width > 0 {
            segment.push(glyph);
            segment.extend(std::iter::repeat_n(bar, segment_width.saturating_sub(1)));
        }
        spans.push(Span::styled(segment, style));
    }

    Line::from(spans)
}

/// A centred explanation of why a region has nothing to show.
///
/// The headline states the observed fact and the body explains what the user
/// can do about it. Neither may describe capability the release lacks.
///
/// `body` is one string: the caller must not hand-break it, because the
/// paragraph is wrapped to whatever width the region turns out to have.
pub(crate) fn empty_state(
    glyph: &str,
    headline: &str,
    body: &str,
    area: Rect,
) -> Paragraph<'static> {
    let mut lines = vec![
        Line::from(Span::styled(glyph.to_owned(), theme::empty_glyph())),
        Line::default(),
        Line::from(Span::styled(headline.to_owned(), theme::empty_headline())),
        Line::default(),
        Line::from(Span::styled(body.to_owned(), theme::empty_body())),
    ];

    // Centre the block vertically against the region it will occupy, without
    // pushing it off a viewport too short to hold it. The measuring paragraph
    // is configured exactly like the rendered one, because wrapping behaviour
    // depends on alignment.
    let paragraph = |lines: Vec<Line<'static>>| {
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .alignment(Alignment::Center)
    };
    let height = paragraph(lines.clone()).line_count(area.width);
    let leading = usize::from(area.height).saturating_sub(height) / 2;
    for _ in 0..leading {
        lines.insert(0, Line::default());
    }
    paragraph(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HINTS: [KeyHint; 3] = [
        KeyHint::new("j/k", "Move"),
        KeyHint::new("Enter", "Register"),
        KeyHint::new("Esc", "Cancel"),
    ];

    const WITH_ESSENTIAL: [KeyHint; 3] = [
        KeyHint::new("j/k", "Move"),
        KeyHint::new("Space", "Include"),
        KeyHint::essential("Esc", "Cancel"),
    ];

    fn rendered(hints: &[KeyHint], width: u16) -> String {
        key_hint_line(hints, width)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn every_tone_carries_a_distinct_glyph_and_colour() {
        let tones = [
            (Tone::Healthy, "✓", theme::GREEN),
            (Tone::Warning, "!", theme::AMBER),
            (Tone::Critical, "×", theme::RED),
            (Tone::Unmanaged, "U", theme::VIOLET),
            (Tone::Inactive, "-", theme::MUTED),
        ];

        for (tone, glyph, colour) in tones {
            let span = badge(tone, "state");
            assert_eq!(span.content, format!("{glyph} state"));
            assert_eq!(span.style.fg, Some(colour));
        }

        let glyphs: Vec<_> = tones.iter().map(|(tone, ..)| tone_glyph(*tone)).collect();
        let mut unique = glyphs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), glyphs.len(), "glyphs must be distinguishable");
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_selected_entry_marks_every_one_of_its_lines_and_bands_them_all() {
        let rows = list_row_lines(
            vec![
                vec![Span::raw("source")],
                vec![Span::raw("/checkout/source")],
                vec![Span::raw("main@0123456")],
            ],
            true,
            30,
        );

        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert!(line_text(row).starts_with("▌ "), "{:?}", line_text(row));
            assert_eq!(row.width(), 30, "{:?}", line_text(row));
            assert_eq!(row.style.bg, theme::selected_row().bg);
        }
    }

    #[test]
    fn an_unselected_entry_keeps_the_marker_column_as_indent_and_no_band() {
        let rows = list_row_lines(
            vec![vec![Span::raw("source")], vec![Span::raw("/checkout")]],
            false,
            30,
        );

        assert_eq!(line_text(&rows[0]), "  source");
        assert_eq!(line_text(&rows[1]), "  /checkout");
        for row in &rows {
            assert_eq!(row.style.bg, None);
        }
    }

    /// A single-line entry is the same row the flat helper produces, so a list
    /// that grows a second line does not also change how its first one reads.
    #[test]
    fn a_single_line_entry_matches_the_flat_row() {
        for selected in [false, true] {
            assert_eq!(
                list_row_lines(vec![vec![Span::raw("source")]], selected, 24),
                vec![list_row(vec![Span::raw("source")], selected, 24)]
            );
        }
    }

    #[test]
    fn focused_pane_header_has_a_text_marker_independent_of_row_selection() {
        let active = focused_pane_header("Repositories", "2 registered", true);
        let inactive = focused_pane_header("Repositories", "2 registered", false);
        let text = |line: &Line<'_>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };

        assert_eq!(text(&active), "▌ Repositories  2 registered");
        // The marker column stays as indent, so the heading holds its column
        // as focus moves.
        assert_eq!(text(&inactive), "  Repositories  2 registered");
        assert_ne!(text(&active), text(&inactive));
    }

    /// A caller bounding a subtitle to its pane has to know what the header
    /// spends around it. That arithmetic lives here, so it is stated here:
    /// a two-cell marker column and a two-cell gap, focused or not.
    #[test]
    fn a_pane_header_spends_a_marker_column_and_a_gap_of_two_always() {
        for focused in [false, true] {
            for (heading, subtitle) in [("Repositories", "2 registered"), ("あ", "")] {
                assert_eq!(
                    focused_pane_header(heading, subtitle, focused).width(),
                    2 + Span::raw(heading).width() + 2 + Span::raw(subtitle).width(),
                    "{heading:?} {subtitle:?} focused={focused}"
                );
            }
        }
    }

    #[test]
    fn dialog_regions_are_bounded_and_non_overlapping() {
        let inner = Rect::new(10, 5, 30, 10);

        let regions = dialog_regions(inner, 11);

        assert_eq!(regions.body, Rect::new(10, 5, 30, 8));
        assert_eq!(regions.divider, Rect::new(10, 13, 30, 1));
        assert_eq!(regions.status, Rect::new(10, 14, 19, 1));
        assert_eq!(regions.actions, Rect::new(29, 14, 11, 1));
        assert_eq!(regions.status.width + regions.actions.width, inner.width);
        assert_eq!(regions.actions.right(), inner.right());
        assert_eq!(regions.actions.bottom(), inner.bottom());
    }

    #[test]
    fn every_tone_glyph_is_a_single_scalar() {
        for tone in [
            Tone::Healthy,
            Tone::Warning,
            Tone::Critical,
            Tone::Unmanaged,
            Tone::Inactive,
        ] {
            assert_eq!(tone_glyph(tone).chars().count(), 1);
        }
    }

    #[test]
    fn every_hint_is_shown_when_the_row_is_wide_enough() {
        let line = rendered(&HINTS, 60);

        assert_eq!(line, " j/k Move   Enter Register   Esc Cancel");
        assert!(!line.contains(OVERFLOW_MARK));
    }

    #[test]
    fn hints_that_do_not_fit_are_dropped_whole() {
        assert_eq!(rendered(&HINTS, 30), " j/k Move   Enter Register …");
        assert_eq!(rendered(&HINTS, 24), " j/k Move …");
    }

    #[test]
    fn a_row_narrower_than_one_hint_shows_only_the_overflow_mark() {
        assert_eq!(rendered(&HINTS, 4), "  …");
    }

    #[test]
    fn essential_hints_are_never_traded_for_ordinary_ones() {
        // Too narrow even for the escape route: show what fits of it rather
        // than filling the row with hints that matter less.
        let line = rendered(&WITH_ESSENTIAL, 12);
        assert!(!line.contains("j/k"), "{line:?}");
    }

    #[test]
    fn the_last_declared_escape_survives_when_essentials_compete() {
        const TWO_WAYS_OUT: [KeyHint; 2] = [
            KeyHint::essential("Enter", "Inspect"),
            KeyHint::essential("Esc", "Cancel"),
        ];

        // Room for one of the two: keep the way out, not the way onward.
        let line = rendered(&TWO_WAYS_OUT, 16);
        assert!(line.contains("Esc Cancel"), "{line:?}");
        assert!(!line.contains("Enter"), "{line:?}");
    }

    #[test]
    fn the_way_out_survives_truncation() {
        // Wide enough for everything.
        assert_eq!(
            rendered(&WITH_ESSENTIAL, 45),
            " j/k Move   Space Include   Esc Cancel"
        );
        // Too narrow: the ordinary hints go, the escape route stays, and the
        // surviving hints keep their declared order.
        assert_eq!(rendered(&WITH_ESSENTIAL, 25), " j/k Move   Esc Cancel …");
        assert_eq!(rendered(&WITH_ESSENTIAL, 16), " Esc Cancel …");
    }

    #[test]
    fn the_rendered_row_never_exceeds_the_available_width() {
        for hints in [HINTS.as_slice(), WITH_ESSENTIAL.as_slice(), [].as_slice()] {
            for width in 1..=60_u16 {
                let line = rendered(hints, width);
                assert!(
                    cell_width(&line) <= usize::from(width),
                    "width {width} produced {line:?}"
                );
            }
        }
    }

    #[test]
    fn no_surviving_hint_is_ever_shown_in_part() {
        for hints in [HINTS.as_slice(), WITH_ESSENTIAL.as_slice()] {
            for width in 1..=60_u16 {
                let line = rendered(hints, width);
                let body = line.trim_start().trim_end_matches('…').trim_end();
                for segment in body.split(HINT_SEPARATOR).filter(|part| !part.is_empty()) {
                    assert!(
                        hints
                            .iter()
                            .any(|hint| segment == format!("{} {}", hint.key, hint.label)),
                        "width {width} produced partial hint {segment:?} in {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_empty_hint_set_renders_nothing_to_read() {
        assert_eq!(rendered(&[], 40), " ");
    }
}
