//! Semantic presentation tokens translated from `spec/tui-prototype.html`.
//!
//! Every colour used by the application is defined here. Screen modules ask for
//! a semantic role rather than a colour so the palette can be retuned, or later
//! degraded for low-colour terminals, in one place.

use ratatui::style::{Color, Modifier, Style};

// Prototype `:root` palette.
pub(crate) const TERMINAL: Color = Color::Rgb(0x0b, 0x0f, 0x14);
pub(crate) const SURFACE: Color = Color::Rgb(0x0f, 0x15, 0x1d);
pub(crate) const SURFACE_2: Color = Color::Rgb(0x12, 0x1a, 0x24);
pub(crate) const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);
pub(crate) const TEXT_STRONG: Color = Color::Rgb(0xf2, 0xf6, 0xfa);
pub(crate) const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);
pub(crate) const FAINT: Color = Color::Rgb(0x53, 0x61, 0x71);
pub(crate) const LINE: Color = Color::Rgb(0x29, 0x34, 0x40);
pub(crate) const LINE_STRONG: Color = Color::Rgb(0x43, 0x52, 0x64);
pub(crate) const TEXT: Color = Color::Rgb(0xd7, 0xde, 0xe7);
pub(crate) const GREEN: Color = Color::Rgb(0x8b, 0xd4, 0x9c);
pub(crate) const AMBER: Color = Color::Rgb(0xe6, 0xbd, 0x6a);
pub(crate) const RED: Color = Color::Rgb(0xee, 0x6b, 0x73);
pub(crate) const CYAN: Color = Color::Rgb(0x73, 0xd7, 0xee);
pub(crate) const VIOLET: Color = Color::Rgb(0xc7, 0x9b, 0xf2);

/// A semantic status role. Every tone is paired with a glyph and a word at the
/// call site so meaning survives a monochrome terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Tone {
    /// Present, valid, and owned by Skilled.
    Healthy,
    /// Usable but needing attention.
    Warning,
    /// Unusable, or blocked from a safe repair.
    Critical,
    /// Present on disk but not owned by Skilled.
    ///
    /// No scanner reports this state yet. The tone is defined now so the
    /// installation-inventory slice inherits a settled presentation instead of
    /// inventing one, and its rendering is covered by component tests.
    #[allow(
        dead_code,
        reason = "installation scanning, which produces this state, is a later slice"
    )]
    Unmanaged,
    /// Absent, or not yet determined.
    Inactive,
}

pub(crate) fn tone_style(tone: Tone) -> Style {
    let colour = match tone {
        Tone::Healthy => GREEN,
        Tone::Warning => AMBER,
        Tone::Critical => RED,
        Tone::Unmanaged => VIOLET,
        Tone::Inactive => FAINT,
    };
    Style::default().fg(colour)
}

/// The canvas the whole application is painted on.
///
/// Skilled owns the full screen, so it paints one surface and lets every other
/// token inherit it. Tokens below carry a background only where it is
/// deliberate local emphasis; otherwise chrome and workspace would disagree on
/// any terminal whose own background is not this colour.
pub(crate) fn app_surface() -> Style {
    Style::default().fg(TEXT).bg(TERMINAL)
}

/// Persistent chrome rows: title bar, navigation, and key hints.
pub(crate) fn chrome() -> Style {
    Style::default().fg(MUTED)
}

/// The product mark in the title bar.
pub(crate) fn product_mark() -> Style {
    Style::default().fg(GREEN)
}

/// The product name in the title bar.
pub(crate) fn product_name() -> Style {
    Style::default()
        .fg(TEXT_STRONG)
        .add_modifier(Modifier::BOLD)
}

/// The band the navigation strip sits on.
pub(crate) fn nav_surface() -> Style {
    Style::default().bg(SURFACE)
}

/// The navigation entry for the view currently on screen.
pub(crate) fn nav_active() -> Style {
    Style::default()
        .fg(TEXT_STRONG)
        .bg(SURFACE_2)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// A navigation entry the user can reach but is not currently viewing.
pub(crate) fn nav_inactive() -> Style {
    Style::default().fg(MUTED).bg(SURFACE)
}

/// The surface behind the focused row of a list.
pub(crate) fn selected_row() -> Style {
    Style::default().bg(SURFACE_3).add_modifier(Modifier::BOLD)
}

/// The marker on the focused row of a list.
pub(crate) fn focus_marker() -> Style {
    Style::default().fg(CYAN)
}

/// The border of a workspace pane.
///
/// Focus is also carried by a marker on the focused row, so the accent colour
/// is reinforcement rather than the only cue.
pub(crate) fn pane_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(CYAN)
    } else {
        Style::default().fg(LINE)
    }
}

/// A section heading inside a pane or dialog body.
pub(crate) fn section_title() -> Style {
    Style::default().fg(CYAN).add_modifier(Modifier::BOLD)
}

/// A setup step that the user has already completed.
pub(crate) fn progress_complete() -> Style {
    Style::default().fg(GREEN)
}

/// The setup step currently on screen.
pub(crate) fn progress_active() -> Style {
    Style::default().fg(CYAN).add_modifier(Modifier::BOLD)
}

/// A setup step that has not been reached yet.
pub(crate) fn progress_pending() -> Style {
    Style::default().fg(FAINT)
}

/// The border of a modal dialog.
pub(crate) fn dialog_border() -> Style {
    Style::default().fg(LINE_STRONG).bg(SURFACE)
}

/// The name of a modal dialog.
pub(crate) fn dialog_title() -> Style {
    Style::default()
        .fg(TEXT_STRONG)
        .bg(SURFACE)
        .add_modifier(Modifier::BOLD)
}

/// The scope note beside a dialog's name.
pub(crate) fn dialog_scope() -> Style {
    Style::default().fg(MUTED).bg(SURFACE)
}

/// The interior of a modal dialog.
pub(crate) fn dialog_surface() -> Style {
    Style::default().fg(TEXT).bg(SURFACE)
}

/// The title of a workspace pane.
pub(crate) fn pane_heading() -> Style {
    Style::default()
        .fg(TEXT_STRONG)
        .add_modifier(Modifier::BOLD)
}

/// The count or qualifier beside a pane title.
pub(crate) fn pane_subtitle() -> Style {
    Style::default().fg(FAINT)
}

/// A horizontal rule separating regions.
pub(crate) fn rule() -> Style {
    Style::default().fg(LINE)
}

/// The glyph above an empty-state headline.
pub(crate) fn empty_glyph() -> Style {
    Style::default().fg(FAINT)
}

/// The observed fact an empty region reports.
pub(crate) fn empty_headline() -> Style {
    Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
}

/// The explanation beneath an empty-state headline.
pub(crate) fn empty_body() -> Style {
    Style::default().fg(MUTED)
}

/// A key cap in the key-hint bar.
pub(crate) fn key_cap() -> Style {
    Style::default()
        .fg(TEXT_STRONG)
        .bg(SURFACE_2)
        .add_modifier(Modifier::BOLD)
}

/// The description beside a key cap.
pub(crate) fn key_label() -> Style {
    Style::default().fg(MUTED)
}

/// A navigation entry the user cannot open right now.
///
/// The faint colour is the only style cue; stacking `DIM` on top pushes the
/// text towards illegibility. The unavailability is spelled out in words
/// beside the entry, so nothing depends on this colour being perceived.
pub(crate) fn nav_disabled() -> Style {
    Style::default().fg(FAINT).bg(SURFACE)
}
