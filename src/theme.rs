//! Semantic presentation tokens translated from `spec/tui-prototype.html`.
//!
//! Every colour used by the application is defined here. Screen modules ask for
//! a semantic role rather than a colour so the palette can be retuned, or later
//! degraded for low-colour terminals, in one place.

use ratatui::style::{Color, Modifier, Style};

// Prototype palette: the `:root` variables plus the selector-local surfaces.
pub(crate) const TERMINAL: Color = Color::Rgb(0x0b, 0x0f, 0x14);
/// Prototype `.terminal-titlebar` / `.keybar` background: the band the
/// persistent chrome rows sit on.
pub(crate) const BAND: Color = Color::Rgb(0x0d, 0x12, 0x18);
/// Prototype `.detail-pane` background: the detail region's surface, in the
/// wide aside and the compact drill-in alike.
pub(crate) const DETAIL_SURFACE: Color = Color::Rgb(0x0c, 0x11, 0x17);
pub(crate) const SURFACE: Color = Color::Rgb(0x0f, 0x15, 0x1d);
pub(crate) const SURFACE_2: Color = Color::Rgb(0x12, 0x1a, 0x24);
pub(crate) const SURFACE_3: Color = Color::Rgb(0x17, 0x21, 0x2c);
pub(crate) const TEXT_STRONG: Color = Color::Rgb(0xf2, 0xf6, 0xfa);
pub(crate) const MUTED: Color = Color::Rgb(0x84, 0x91, 0xa1);
/// Below 4.5:1 on every surface; reserved for decorative or WCAG-exempt
/// roles. Information-bearing text uses `MUTED` or brighter.
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
    /// Present on disk and structurally sound, but not owned by Skilled.
    ///
    /// Distinct from every other tone because unmanaged content is neither a
    /// problem to fix nor an installation to claim; it is observed and left
    /// alone.
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
        // An absent or undetermined status is still information; MUTED keeps
        // it above the 4.5:1 threshold on every surface, including selected
        // rows, where the prototype's faint grey falls to about 2.6:1.
        Tone::Inactive => MUTED,
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

/// The band the title bar and key-hint bar sit on (prototype
/// `.terminal-titlebar` and `.keybar`).
///
/// Background only, mirroring `nav_surface()`: the text on it keeps its own
/// role — `chrome()` and `key_label()` inherit the band, `key_cap()` carries
/// its own local emphasis.
pub(crate) fn chrome_band() -> Style {
    Style::default().bg(BAND)
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

/// The count beside a navigation entry (prototype `.tab-count`).
///
/// No background of its own: the count sits inside an entry and inherits that
/// entry's surface, so it reads the same on an active (`SURFACE_2`) and an
/// inactive (`SURFACE`) tab. It inherits the active entry's underline for the
/// same reason — the prototype draws that as a border under the whole tab,
/// count included — but it drops the bold, which is emphasis on the
/// destination's name rather than on the number beside it.
///
/// The textual '·' lead-in is decided at the render site, not here: this
/// token styles the count and the dot together, the glyph is what keeps a
/// '·N' from reading as a route key in a row that already carries one.
///
/// A count may only be rendered when the underlying condition it summarises
/// actually holds; this token styles it, it does not license it.
pub(crate) fn nav_count() -> Style {
    Style::default().fg(AMBER).remove_modifier(Modifier::BOLD)
}

/// The surface behind the focused row of a list.
pub(crate) fn selected_row() -> Style {
    Style::default().bg(SURFACE_3).add_modifier(Modifier::BOLD)
}

/// The marker on the focused row of a list.
pub(crate) fn focus_marker() -> Style {
    Style::default().fg(CYAN)
}

/// A section heading inside a pane or dialog body.
pub(crate) fn section_title() -> Style {
    Style::default().fg(CYAN).add_modifier(Modifier::BOLD)
}

/// A section heading inside the detail region (prototype `.detail-kicker`).
///
/// The prototype sets its kickers in faint grey; a heading that names the
/// section under it is information-bearing, so this deviates to `MUTED` to
/// hold 4.5:1, the same reasoning recorded for the inventory table headings.
/// Cyan stays reserved for focus and selection accents inside a pane, which is
/// why this is a role of its own rather than [`section_title()`].
pub(crate) fn detail_section_title() -> Style {
    Style::default().fg(MUTED)
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
///
/// The names of the remaining steps are information, so this deviates from the
/// prototype's faint grey to stay readable on the dialog surface.
pub(crate) fn progress_pending() -> Style {
    Style::default().fg(MUTED)
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

/// The detail region's surface (prototype `.detail-pane`).
///
/// Carried by the wide-viewport aside and by the compact drill-in alike: the
/// prototype's narrow layout keeps `.detail-pane`'s background too. Background
/// only, like `chrome_band()`: the rows inside keep their own foreground roles
/// and inherit this surface.
pub(crate) fn detail_surface() -> Style {
    Style::default().bg(DETAIL_SURFACE)
}

/// The title of a workspace pane.
pub(crate) fn pane_heading() -> Style {
    Style::default()
        .fg(TEXT_STRONG)
        .add_modifier(Modifier::BOLD)
}

/// The count or qualifier beside a pane title, and detail-field labels.
///
/// These carry real status ("not scanned", counts), so this deviates from the
/// prototype's faint grey to meet 4.5:1 on the terminal surface.
pub(crate) fn pane_subtitle() -> Style {
    Style::default().fg(MUTED)
}

/// The line that names the group a run of list rows belongs to (prototype
/// `.catalog-title`).
///
/// The prototype tints its group headers with a band of their own, so this
/// takes the band the persistent chrome already uses rather than minting a
/// colour: one band across the application, whether it carries chrome or a
/// group header. The text on it names the group and says how its contents are
/// classified, which is information, so it is `MUTED` rather than the
/// prototype's faint grey.
///
/// The tint is barely above the terminal background — as it is in the
/// prototype, whose `.catalog-title` sits at `#0b1016` against `#0b0f14` — so
/// it is a surface, not a cue. What separates a group label from the rows
/// under it is that the label is muted and starts flush where the rows are
/// indented past their marker column.
pub(crate) fn group_label() -> Style {
    Style::default().fg(MUTED).bg(BAND)
}

/// A horizontal rule separating regions.
pub(crate) fn rule() -> Style {
    Style::default().fg(LINE)
}

/// The glyph above an empty-state headline.
///
/// Accepted contrast deviation (skilled-2k3.13.6): the glyph is decoration;
/// the headline and body beside it carry the meaning, so WCAG's 4.5:1
/// body-text threshold does not apply and the prototype's faint grey stays.
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

/// The note beside a keyboard owner's name in the navigation row.
///
/// It explains why the destination shortcuts are absent, which is status the
/// user must be able to read, so it uses the readable muted tone rather than
/// the disabled-entry grey.
pub(crate) fn nav_note() -> Style {
    Style::default().fg(MUTED).bg(SURFACE)
}

/// A navigation entry the user cannot open right now.
///
/// The faint colour is the only style cue; stacking `DIM` on top pushes the
/// text towards illegibility. The unavailability is spelled out in words
/// beside the entry, so nothing depends on this colour being perceived.
///
/// Accepted contrast deviation (skilled-2k3.13.6): WCAG 1.4.3 exempts text in
/// inactive interface components, and a compliant grey would blur the
/// distinction from reachable `MUTED` entries, so the faint grey stays.
pub(crate) fn nav_disabled() -> Style {
    Style::default().fg(FAINT).bg(SURFACE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.x relative luminance of an sRGB colour.
    fn relative_luminance(colour: Color) -> f64 {
        let Color::Rgb(red, green, blue) = colour else {
            panic!("theme colours are RGB; got {colour:?}");
        };
        let linear = |channel: u8| {
            let srgb = f64::from(channel) / 255.0;
            if srgb <= 0.039_28 {
                srgb / 12.92
            } else {
                ((srgb + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
    }

    /// WCAG 2.x contrast ratio between two colours, from 1.0 to 21.0.
    fn contrast_ratio(foreground: Color, background: Color) -> f64 {
        let lighter = relative_luminance(foreground).max(relative_luminance(background));
        let darker = relative_luminance(foreground).min(relative_luminance(background));
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Every surface a span of workspace or dialog text can sit on. Selected
    /// rows put `SURFACE_3` under any tone, so it is the binding constraint;
    /// `BAND` and `DETAIL_SURFACE` sit between `TERMINAL` and `SURFACE` in
    /// luminance.
    const SURFACES: [Color; 6] = [
        TERMINAL,
        BAND,
        DETAIL_SURFACE,
        SURFACE,
        SURFACE_2,
        SURFACE_3,
    ];

    /// A role that paints its own background is read against it; one that
    /// inherits is read against every surface it can land on.
    fn assert_readable(role: &str, style: Style) {
        let foreground = style
            .fg
            .expect("information-bearing roles set a foreground");
        let backgrounds = match style.bg {
            Some(background) => vec![background],
            None => SURFACES.to_vec(),
        };
        for background in backgrounds {
            let ratio = contrast_ratio(foreground, background);
            assert!(
                ratio >= 4.5,
                "{role} is {ratio:.2}:1 against {background:?}; \
                 information-bearing text needs 4.5:1"
            );
        }
    }

    /// Acceptance criterion of skilled-2k3.13.6: text that carries information
    /// meets WCAG 4.5:1 against its surface. `empty_glyph` and `nav_disabled`
    /// are exempt by decision recorded on their doc comments.
    #[test]
    fn information_bearing_text_meets_wcag_contrast_against_its_surface() {
        for tone in [
            Tone::Healthy,
            Tone::Warning,
            Tone::Critical,
            Tone::Unmanaged,
            Tone::Inactive,
        ] {
            assert_readable(&format!("tone_style({tone:?})"), tone_style(tone));
        }
        for (role, style) in [
            ("app_surface()", app_surface()),
            ("chrome()", chrome()),
            ("product_mark()", product_mark()),
            ("product_name()", product_name()),
            ("nav_active()", nav_active()),
            ("nav_inactive()", nav_inactive()),
            ("nav_count()", nav_count()),
            ("nav_note()", nav_note()),
            ("focus_marker()", focus_marker()),
            ("section_title()", section_title()),
            ("detail_section_title()", detail_section_title()),
            ("progress_complete()", progress_complete()),
            ("progress_active()", progress_active()),
            ("progress_pending()", progress_pending()),
            ("dialog_title()", dialog_title()),
            ("dialog_scope()", dialog_scope()),
            ("dialog_surface()", dialog_surface()),
            ("pane_heading()", pane_heading()),
            ("pane_subtitle()", pane_subtitle()),
            ("group_label()", group_label()),
            ("empty_headline()", empty_headline()),
            ("empty_body()", empty_body()),
            ("key_cap()", key_cap()),
            ("key_label()", key_label()),
        ] {
            assert_readable(role, style);
        }
    }

    /// A role that only paints a background must paint one of the surfaces
    /// the readability sweep covers, or text landing on it would escape the
    /// contrast guarantee.
    #[test]
    fn background_roles_paint_covered_surfaces() {
        for (role, style) in [
            ("nav_surface()", nav_surface()),
            ("chrome_band()", chrome_band()),
            ("detail_surface()", detail_surface()),
            ("selected_row()", selected_row()),
        ] {
            let background = style.bg.expect("surface roles set a background");
            assert!(
                SURFACES.contains(&background),
                "{role} paints {background:?}, which the readability test does not cover"
            );
        }
    }
}
