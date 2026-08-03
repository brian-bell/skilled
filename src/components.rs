//! Shared presentation primitives translated from `spec/tui-prototype.html`.
//!
//! Components are pure: they build [`Line`]s and [`Span`]s from data the caller
//! already holds. Nothing here reads the filesystem, the database, or the
//! terminal.

use ratatui::{
    text::{Line, Span},
    widgets::{Block, Borders, Padding},
};

use crate::theme::{self, Tone};

/// One advertised command in the persistent key-hint bar.
///
/// A hint may only be constructed for a command the active context actually
/// handles; the bar is a contract with the user, not decoration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KeyHint {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
}

impl KeyHint {
    pub(crate) const fn new(key: &'static str, label: &'static str) -> Self {
        Self { key, label }
    }

    fn width(self) -> usize {
        self.key.chars().count() + 1 + self.label.chars().count()
    }
}

const HINT_SEPARATOR: &str = "   ";
const OVERFLOW_MARK: &str = "…";

/// Lay out as many complete hints as fit.
///
/// Hints are dropped whole. A clipped hint would advertise a command the user
/// cannot read, so the bar ends with an overflow mark instead.
pub(crate) fn key_hint_line(hints: &[KeyHint], width: u16) -> Line<'static> {
    let width = usize::from(width);
    if width == 0 {
        return Line::default();
    }

    const LEADING: usize = 1;
    let required = LEADING
        + hints
            .iter()
            .enumerate()
            .map(|(index, hint)| hint.width() + if index == 0 { 0 } else { HINT_SEPARATOR.len() })
            .sum::<usize>();
    let complete = required <= width;
    // When something must be dropped, keep a column free for the mark.
    let budget = if complete {
        width
    } else {
        width.saturating_sub(OVERFLOW_MARK.chars().count())
    };

    let mut spans = vec![Span::styled(" ", theme::chrome())];
    let mut used = LEADING;
    for (index, hint) in hints.iter().enumerate() {
        let separator = if index == 0 { 0 } else { HINT_SEPARATOR.len() };
        if used + separator + hint.width() > budget {
            break;
        }
        if index > 0 {
            spans.push(Span::styled(HINT_SEPARATOR, theme::chrome()));
        }
        spans.push(Span::styled(hint.key, theme::key_cap()));
        spans.push(Span::styled(" ", theme::chrome()));
        spans.push(Span::styled(hint.label, theme::key_label()));
        used += separator + hint.width();
    }

    if !complete && used + OVERFLOW_MARK.chars().count() <= width {
        spans.push(Span::styled(OVERFLOW_MARK, theme::key_label()));
    }
    Line::from(spans)
}

/// The glyph that carries a tone's meaning without colour.
///
/// Glyphs avoid East-Asian-ambiguous width so a status column keeps its
/// alignment across terminals.
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

/// The marker that identifies the focused row in a list.
const FOCUS_MARKER: &str = "▌";

/// A selectable row in a list.
///
/// The prototype signals selection with a tinted background and an inset
/// accent bar. A terminal cannot rely on either alone, so focus is carried by a
/// leading marker and bold text as well.
pub(crate) fn list_row(content: Vec<Span<'static>>, selected: bool) -> Line<'static> {
    let mut spans = vec![Span::styled(
        if selected { FOCUS_MARKER } else { " " },
        theme::focus_marker(),
    )];
    spans.push(Span::raw(" "));
    spans.extend(content.into_iter().map(|span| {
        if selected {
            let style = span.style.add_modifier(ratatui::style::Modifier::BOLD);
            span.patch_style(style)
        } else {
            span
        }
    }));
    Line::from(spans)
}

/// A pane heading with a subtitle that quantifies what the pane contains.
pub(crate) fn pane_header(heading: &str, subtitle: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(heading.to_owned(), theme::pane_heading()),
        Span::styled(format!("  {subtitle}"), theme::pane_subtitle()),
    ])
}

/// A centred explanation of why a region has nothing to show.
///
/// The headline states the observed fact and the body explains what the user
/// can do about it. Neither may describe capability the release lacks.
pub(crate) fn empty_state(
    glyph: &str,
    headline: &str,
    body: &[&str],
    height: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(glyph.to_owned(), theme::empty_glyph())).centered(),
        Line::default(),
        Line::from(Span::styled(headline.to_owned(), theme::empty_headline())).centered(),
        Line::default(),
    ];
    lines.extend(
        body.iter().map(|line| {
            Line::from(Span::styled((*line).to_owned(), theme::empty_body())).centered()
        }),
    );

    // Centre the block vertically without pushing it off a short viewport.
    let leading = usize::from(height).saturating_sub(lines.len()) / 2;
    let mut centred = vec![Line::default(); leading];
    centred.extend(lines);
    centred
}

#[cfg(test)]
mod tests {
    use super::*;

    const HINTS: [KeyHint; 3] = [
        KeyHint::new("j/k", "Move"),
        KeyHint::new("Enter", "Register"),
        KeyHint::new("Esc", "Cancel"),
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
            (Tone::Inactive, "-", theme::FAINT),
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

    #[test]
    fn tone_glyphs_occupy_one_column() {
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
        // Thirty columns hold the first two hints but not the third.
        assert_eq!(rendered(&HINTS, 30), " j/k Move   Enter Register…");
        // Twenty-four hold only the first.
        assert_eq!(rendered(&HINTS, 24), " j/k Move…");
    }

    #[test]
    fn a_row_narrower_than_one_hint_shows_only_the_overflow_mark() {
        assert_eq!(rendered(&HINTS, 4), " …");
    }

    #[test]
    fn the_rendered_row_never_exceeds_the_available_width() {
        for width in 1..=60_u16 {
            let line = rendered(&HINTS, width);
            assert!(
                line.chars().count() <= usize::from(width),
                "width {width} produced {line:?}"
            );
        }
    }

    #[test]
    fn an_empty_hint_set_renders_nothing_to_read() {
        assert_eq!(rendered(&[], 40), " ");
    }
}
