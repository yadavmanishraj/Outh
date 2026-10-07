//! Design system for Outh (REDESIGN_SPEC §1 / AUDIT_1 §G).
//!
//! Every font size, weight, opacity and spacing value in the app comes
//! from this module — pages never write literals. Colors come only from
//! `ThemeBrush` (no hard-coded colors anywhere), so the app follows the
//! Windows light/dark theme by construction.

use windows_reactor::*;

use crate::NoteSeverity;

// ---------------------------------------------------------------------------
// Type tokens (AUDIT_1 §G type ramp; Segoe UI Variable is inherited from
// the platform — never set a custom family on Windows).
// ---------------------------------------------------------------------------

pub const TYPE_PAGE_TITLE: f64 = 28.0;
pub const TYPE_SECTION: f64 = 20.0;
pub const TYPE_STRONG: f64 = 14.0;
pub const TYPE_BODY: f64 = 14.0;
pub const TYPE_CAPTION: f64 = 12.0;

/// Dimming tiers (workaround for the missing secondary-text theme brush).
/// `DIM_SECONDARY` is for descriptions/helper text at body size;
/// `DIM_TERTIARY` is for caption-size metadata only.
pub const DIM_SECONDARY: f64 = 0.76;
pub const DIM_TERTIARY: f64 = 0.60;

// ---------------------------------------------------------------------------
// Spacing (4/8 grid) and page geometry.
// ---------------------------------------------------------------------------

pub const SPACE_XS: f64 = 4.0;
pub const SPACE_S: f64 = 8.0;
pub const SPACE_M: f64 = 12.0;
pub const SPACE_L: f64 = 16.0;
pub const SPACE_XL: f64 = 24.0;
#[allow(dead_code)]
pub const SPACE_XXL: f64 = 32.0;

pub const PAGE_PADDING_X: f64 = 24.0;
pub const PAGE_PADDING_TOP: f64 = 32.0;
/// Documented content cap for very wide windows. Not currently applied:
/// page scaffolds stretch to the content viewport instead (the R-10 fix —
/// a Left-aligned max-width column measured narrower than the viewport).
/// Kept as the scale's reference value if a capped layout returns.
#[allow(dead_code)]
pub const CONTENT_MAX_WIDTH: f64 = 980.0;

// ---------------------------------------------------------------------------
// Text helpers — one function per type token.
// ---------------------------------------------------------------------------

/// Page header: 28 SemiBold, exactly one per page.
pub fn page_title(text: impl AsRef<str>) -> View {
    TextBlock::new()
        .text(text)
        .font_size(TYPE_PAGE_TITLE)
        .font_weight(FontWeight::SEMI_BOLD)
        .automation_heading_level(AutomationHeadingLevel::Level1)
        .into()
}

/// Section header: 20 SemiBold (card groups, page-level sections).
pub fn section_title(text: impl AsRef<str>) -> View {
    TextBlock::new()
        .text(text)
        .font_size(TYPE_SECTION)
        .font_weight(FontWeight::SEMI_BOLD)
        .automation_heading_level(AutomationHeadingLevel::Level2)
        .into()
}

/// Row/card title: 14 SemiBold.
pub fn strong(text: impl AsRef<str>) -> View {
    TextBlock::new()
        .text(text)
        .font_size(TYPE_STRONG)
        .font_weight(FontWeight::SEMI_BOLD)
        .text_wrapping(TextWrapping::Wrap)
        .into()
}

/// Default text: 14 Normal, full-strength.
pub fn body(text: impl AsRef<str>) -> View {
    TextBlock::new()
        .text(text)
        .font_size(TYPE_BODY)
        .font_weight(FontWeight::NORMAL)
        .text_wrapping(TextWrapping::Wrap)
        .into()
}

/// Secondary text: body size at `DIM_SECONDARY` (descriptions, helpers).
pub fn secondary(text: impl AsRef<str>) -> View {
    TextBlock::new()
        .text(text)
        .font_size(TYPE_BODY)
        .font_weight(FontWeight::NORMAL)
        .text_wrapping(TextWrapping::Wrap)
        .opacity(DIM_SECONDARY)
        .into()
}

/// Metadata: 12 Normal at `DIM_TERTIARY` (byte counts, hints, footnotes).
pub fn caption(text: impl AsRef<str>) -> View {
    TextBlock::new()
        .text(text)
        .font_size(TYPE_CAPTION)
        .font_weight(FontWeight::NORMAL)
        .text_wrapping(TextWrapping::Wrap)
        .opacity(DIM_TERTIARY)
        .into()
}

// ---------------------------------------------------------------------------
// Surfaces & component recipes.
// ---------------------------------------------------------------------------

/// The one card recipe: CardBackground + CardStroke 1px + radius 8 +
/// 16px padding, children stacked with 12px gaps (SPACE_M).
pub fn card(children: Vec<View>) -> View {
    Border::new()
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(CornerRadius::uniform(8.0))
        .padding(Thickness::uniform(SPACE_L))
        .content(StackPanel::new().spacing(SPACE_M).children(children))
        .into()
}

/// Settings row (Fluent settings pattern): title (Strong) + description
/// (Body, dimmed) on the left, the bare control right-aligned and
/// vertically centred. Controls carry no header/on/off text of their own.
pub fn settings_row(title: &str, description: &str, control: View) -> View {
    Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(SPACE_L)
        .children(vec![
            StackPanel::new()
                .spacing(SPACE_XS)
                .children(vec![strong(title), secondary(description)])
                .into(),
            // The control arrives as an erased `View`, which cannot take
            // grid placement itself — wrap it in a transparent Border that
            // carries the column + centring.
            Border::new()
                .grid_column(1)
                .vertical_alignment(VerticalAlignment::Center)
                .content(control)
                .into(),
        ])
        .into()
}

/// Maps the app's note severities onto InfoBar severities.
fn info_bar_severity(severity: NoteSeverity) -> InfoBarSeverity {
    match severity {
        NoteSeverity::Info => InfoBarSeverity::Informational,
        NoteSeverity::Success => InfoBarSeverity::Success,
        NoteSeverity::Warning => InfoBarSeverity::Warning,
        NoteSeverity::Error => InfoBarSeverity::Error,
    }
}

/// The single feedback component (spec §2): an InfoBar with an optional
/// title and an optional close callback (present ⇒ closable). InfoBar has
/// no action-button slot in this stack — actions live in the page.
pub fn info_bar(
    severity: NoteSeverity,
    title: Option<&str>,
    message: &str,
    on_close: Option<Callback<()>>,
) -> View {
    let mut bar = InfoBar::new()
        .severity(info_bar_severity(severity))
        .is_open(true)
        .is_closable(on_close.is_some())
        .message(message);
    if let Some(title) = title {
        bar = bar.title(title);
    }
    if let Some(on_close) = on_close {
        bar = bar.on_closed(on_close);
    }
    bar.into()
}

/// Renders a `Note` as the standard closable InfoBar for its page.
pub fn note_bar(note: &crate::Note, on_close: Callback<()>) -> View {
    info_bar(note.severity, None, &note.text, Some(on_close))
}

/// Centred empty state: large dimmed symbol, section title, description,
/// and one action (typically an Accent button built by the caller).
pub fn empty_state(symbol: Symbol, title: &str, description: &str, action: View) -> View {
    StackPanel::new()
        .spacing(SPACE_S)
        .children(vec![
            // Neither SymbolIcon nor FontIcon exposes a font-size builder
            // in this stack; the element box is the only sizing knob.
            SymbolIcon::new()
                .symbol(symbol)
                .width(40.0)
                .height(40.0)
                .opacity(DIM_TERTIARY)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into(),
            // Same tokens as section_title()/secondary(), plus centring —
            // alignment can't be added to an already-erased View.
            TextBlock::new()
                .text(title)
                .font_size(TYPE_SECTION)
                .font_weight(FontWeight::SEMI_BOLD)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into(),
            TextBlock::new()
                .text(description)
                .font_size(TYPE_BODY)
                .font_weight(FontWeight::NORMAL)
                .text_wrapping(TextWrapping::Wrap)
                .opacity(DIM_SECONDARY)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into(),
            // The action is an erased `View`; centre it via a wrapper.
            Border::new()
                .horizontal_alignment(HorizontalAlignment::Center)
                .content(action)
                .into(),
        ])
        .into()
}

// ---------------------------------------------------------------------------
// Formatting.
// ---------------------------------------------------------------------------

/// Human byte counts ("3.2 MB"), binary units as in gotohp's GUI.
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
