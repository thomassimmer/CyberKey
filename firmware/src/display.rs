//! Drawing helpers for the M5StickC Plus 2 display (ST7789V2).
//!
//! Logical frame: 240×135 landscape (after Rotation::Deg90).
//! Top 20 px: persistent status bar — time (yellow, left) + battery (cyan, right).
//! Content area: y = 20..135 (115 px tall).

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_10X20, FONT_5X8, FONT_6X10, FONT_6X13},
        MonoTextStyle,
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Alignment, Text, TextStyleBuilder},
};

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

pub const W: u32 = 240;
pub const H: u32 = 135;

const BAR_H: u32 = 20;
const CONTENT_Y: i32 = BAR_H as i32;
// Vertical centre of the content area.
const CONTENT_CY: i32 = CONTENT_Y + (H as i32 - BAR_H as i32) / 2; // 77

// ---------------------------------------------------------------------------
// Cyberpunk 2077 palette
// ---------------------------------------------------------------------------

const NEON_YELLOW: Rgb565 = Rgb565::YELLOW;
const NEON_CYAN: Rgb565 = Rgb565::CYAN;
const NEON_GREEN: Rgb565 = Rgb565::GREEN;
const NEON_RED: Rgb565 = Rgb565::RED;

// ---------------------------------------------------------------------------
// Status-bar data
// ---------------------------------------------------------------------------

/// Updated each main-loop cycle and passed to every draw call.
pub struct StatusBar<'a> {
    /// Formatted time, e.g. `"23:14"`.
    pub time: &'a str,
    /// Battery percentage 0–100, or `None` when the ADC read failed.
    pub battery: Option<u8>,
}

impl StatusBar<'static> {
    /// Placeholder before real data is available.
    pub fn unknown() -> Self {
        StatusBar {
            time: "--:--",
            battery: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn clear_content<D: DrawTarget<Color = Rgb565>>(d: &mut D) {
    let _ = Rectangle::new(Point::new(0, CONTENT_Y), Size::new(W, H - BAR_H))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(d);
}

fn ts_center() -> embedded_graphics::text::TextStyle {
    TextStyleBuilder::new().alignment(Alignment::Center).build()
}

fn ts_right() -> embedded_graphics::text::TextStyle {
    TextStyleBuilder::new().alignment(Alignment::Right).build()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Redraw only the top status bar — called on every state transition and when
/// the minute changes, without touching the content area.
pub fn update_topbar<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>) {
    // Background
    let _ = Rectangle::new(Point::new(0, 0), Size::new(W, BAR_H))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(d);

    // Time — yellow, left-aligned; baseline at y=13 gives 3 px top padding.
    let _ = Text::new(
        sb.time,
        Point::new(3, 13),
        MonoTextStyle::new(&FONT_6X10, NEON_YELLOW),
    )
    .draw(d);

    // Battery — cyan (red when ≤ 20 %, dim when unknown), right-aligned.
    let (bat_str, bat_color) = match sb.battery {
        Some(pct) => (
            format!("{:3}%", pct),
            if pct <= 20 { NEON_RED } else { NEON_CYAN },
        ),
        None => (" --%".to_string(), Rgb565::CSS_DARK_GRAY),
    };
    let _ = Text::with_text_style(
        &bat_str,
        Point::new(W as i32 - 3, 13),
        MonoTextStyle::new(&FONT_6X10, bat_color),
        ts_right(),
    )
    .draw(d);

    // Separator line
    let _ = Line::new(
        Point::new(0, BAR_H as i32 - 1),
        Point::new(W as i32 - 1, BAR_H as i32 - 1),
    )
    .into_styled(PrimitiveStyle::with_stroke(NEON_CYAN, 1))
    .draw(d);
}

/// Idle screen: BLE pairing PIN.
///
/// ```text
/// >> BT PAIRING <<
///    XXX XXX
///  enter on host
/// ```
pub fn show_pin<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, pin: u32) {
    clear_content(d);
    update_topbar(d, sb);
    let cx = (W / 2) as i32;
    let _ = Text::with_text_style(
        ">> BT PAIRING <<",
        Point::new(cx, CONTENT_CY - 22),
        MonoTextStyle::new(&FONT_6X13, NEON_CYAN),
        ts_center(),
    )
    .draw(d);
    let pin_str = format!("{:03} {:03}", pin / 1000, pin % 1000);
    let _ = Text::with_text_style(
        &pin_str,
        Point::new(cx, CONTENT_CY + 4),
        MonoTextStyle::new(&FONT_10X20, NEON_YELLOW),
        ts_center(),
    )
    .draw(d);
    let _ = Text::with_text_style(
        "enter on host",
        Point::new(cx, CONTENT_CY + 26),
        MonoTextStyle::new(&FONT_5X8, Rgb565::CSS_LIGHT_GRAY),
        ts_center(),
    )
    .draw(d);
}

/// Single-line status message centred in content area.
pub fn show_status<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, msg: &str) {
    clear_content(d);
    update_topbar(d, sb);
    let _ = Text::with_text_style(
        msg,
        Point::new((W / 2) as i32, CONTENT_CY),
        MonoTextStyle::new(&FONT_10X20, NEON_CYAN),
        ts_center(),
    )
    .draw(d);
}

/// Two-line status message centred in content area.
pub fn show_status_2line<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    sb: &StatusBar<'_>,
    line1: &str,
    line2: &str,
) {
    clear_content(d);
    update_topbar(d, sb);
    let cx = (W / 2) as i32;
    let style = MonoTextStyle::new(&FONT_6X13, NEON_CYAN);
    let _ =
        Text::with_text_style(line1, Point::new(cx, CONTENT_CY - 8), style, ts_center()).draw(d);
    let _ =
        Text::with_text_style(line2, Point::new(cx, CONTENT_CY + 8), style, ts_center()).draw(d);
}

/// Successful fingerprint match.
pub fn show_auth_ok<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, page_id: u16) {
    clear_content(d);
    update_topbar(d, sb);
    let cx = (W / 2) as i32;
    let _ = Text::with_text_style(
        "AUTH OK",
        Point::new(cx, CONTENT_CY - 12),
        MonoTextStyle::new(&FONT_10X20, NEON_GREEN),
        ts_center(),
    )
    .draw(d);
    let _ = Text::with_text_style(
        &format!("ID: {}", page_id),
        Point::new(cx, CONTENT_CY + 12),
        MonoTextStyle::new(&FONT_10X20, NEON_CYAN),
        ts_center(),
    )
    .draw(d);
}

/// Successful enrollment.
pub fn show_enroll_ok<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, slot: u16) {
    clear_content(d);
    update_topbar(d, sb);
    let cx = (W / 2) as i32;
    let _ = Text::with_text_style(
        "ENROLLED",
        Point::new(cx, CONTENT_CY - 12),
        MonoTextStyle::new(&FONT_10X20, NEON_GREEN),
        ts_center(),
    )
    .draw(d);
    let _ = Text::with_text_style(
        &format!("SLOT {}", slot),
        Point::new(cx, CONTENT_CY + 12),
        MonoTextStyle::new(&FONT_10X20, NEON_CYAN),
        ts_center(),
    )
    .draw(d);
}

/// Factory-reset confirmation screen.
pub fn show_reset_ok<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>) {
    clear_content(d);
    update_topbar(d, sb);
    let cx = (W / 2) as i32;
    let _ = Text::with_text_style(
        "RESET OK",
        Point::new(cx, CONTENT_CY - 12),
        MonoTextStyle::new(&FONT_10X20, NEON_GREEN),
        ts_center(),
    )
    .draw(d);
    let _ = Text::with_text_style(
        "REBOOTING...",
        Point::new(cx, CONTENT_CY + 12),
        MonoTextStyle::new(&FONT_6X13, NEON_CYAN),
        ts_center(),
    )
    .draw(d);
}

/// No fingerprint match.
pub fn show_no_match<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>) {
    clear_content(d);
    update_topbar(d, sb);
    let _ = Text::with_text_style(
        "NO MATCH",
        Point::new((W / 2) as i32, CONTENT_CY),
        MonoTextStyle::new(&FONT_10X20, NEON_RED),
        ts_center(),
    )
    .draw(d);
}

/// TOTP code display.
///
/// ```text
/// >> TOTP <<
///  XXX XXX
/// ```
pub fn show_totp<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, code: u32) {
    clear_content(d);
    update_topbar(d, sb);
    let cx = (W / 2) as i32;
    let _ = Text::with_text_style(
        ">> TOTP <<",
        Point::new(cx, CONTENT_CY - 20),
        MonoTextStyle::new(&FONT_6X13, NEON_CYAN),
        ts_center(),
    )
    .draw(d);
    let _ = Text::with_text_style(
        &format!("{:03} {:03}", code / 1000, code % 1000),
        Point::new(cx, CONTENT_CY + 8),
        MonoTextStyle::new(&FONT_10X20, NEON_GREEN),
        ts_center(),
    )
    .draw(d);
}
