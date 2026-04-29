//! Drawing helpers for the M5StickC Plus 2 display (ST7789V2).
//!
//! Logical frame: 240×135 landscape (after Rotation::Deg90).
//! Top 20 px: persistent status bar — time (yellow, left) + battery (cyan, right).
//! Content area: y = 20..135 (115 px tall).

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_5X8, FONT_6X10},
        MonoTextStyle,
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Alignment, Text, TextStyleBuilder},
};

use crate::fonts::orbitron_font::{draw_text_prop, get_text_width};
use crate::fonts::orbitron_mini::{draw_text_prop as draw_mini, get_text_width as get_mini_width};
use crate::fonts::orbitron_large::{draw_text_prop as draw_large, get_text_width as get_large_width};

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

pub const W: u32 = 240;
pub const H: u32 = 135;

const BAR_H: u32 = 30;
const CONTENT_Y: i32 = BAR_H as i32;
// Vertical centre of the content area.
const CONTENT_CY: i32 = CONTENT_Y + (H as i32 - BAR_H as i32) / 2; // ~82

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

fn draw_center<D: DrawTarget<Color = Rgb565>>(d: &mut D, text: &str, y: i32, color: Rgb565) {
    let w = get_text_width(text);
    let x = (W as i32 - w) / 2;
    let _ = draw_text_prop(d, text, Point::new(x, y), color);
}

fn draw_right<D: DrawTarget<Color = Rgb565>>(d: &mut D, text: &str, y: i32, color: Rgb565) {
    let w = get_text_width(text);
    let x = W as i32 - w - 5;
    let _ = draw_text_prop(d, text, Point::new(x, y), color);
}

fn draw_large_center<D: DrawTarget<Color = Rgb565>>(d: &mut D, text: &str, y: i32, color: Rgb565) {
    let w = get_large_width(text);
    let x = (W as i32 - w) / 2;
    let _ = draw_large(d, text, Point::new(x, y), color);
}

fn draw_mini_right<D: DrawTarget<Color = Rgb565>>(d: &mut D, text: &str, y: i32, color: Rgb565) {
    let w = get_mini_width(text);
    let x = W as i32 - w - 5;
    let _ = draw_mini(d, text, Point::new(x, y), color);
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

    // Time — yellow, left-aligned;
    let _ = draw_mini(d, sb.time, Point::new(5, 7), NEON_YELLOW);

    // Battery — cyan (red when ≤ 20 %, dim when unknown), right-aligned.
    let (bat_str, bat_color) = match sb.battery {
        Some(pct) => (
            format!("{}%", pct),
            if pct <= 20 { NEON_RED } else { NEON_CYAN },
        ),
        None => ("--%".to_string(), Rgb565::CSS_DARK_GRAY),
    };
    draw_mini_right(d, &bat_str, 7, bat_color);

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
    draw_center(d, ">> BT PAIRING <<", CONTENT_CY - 40, NEON_CYAN);
    
    let pin_str = format!("{:03} {:03}", pin / 1000, pin % 1000);
    draw_large_center(d, &pin_str, CONTENT_CY - 5, NEON_YELLOW);

    let _ = Text::new(
        "ENTER ON HOST",
        Point::new((W / 2) as i32 - 40, CONTENT_CY + 40),
        MonoTextStyle::new(&FONT_5X8, Rgb565::CSS_LIGHT_GRAY),
    )
    .draw(d);
}

/// Single-line status message centred in content area.
pub fn show_status<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, msg: &str) {
    clear_content(d);
    update_topbar(d, sb);
    draw_center(d, &msg.to_uppercase(), CONTENT_CY - 10, NEON_CYAN);
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
    draw_center(d, &line1.to_uppercase(), CONTENT_CY - 20, NEON_CYAN);
    draw_center(d, &line2.to_uppercase(), CONTENT_CY + 10, NEON_CYAN);
}

/// Successful fingerprint match.
pub fn show_auth_ok<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, page_id: u16) {
    clear_content(d);
    update_topbar(d, sb);
    draw_center(d, "AUTH OK", CONTENT_CY - 20, NEON_GREEN);
    draw_center(d, &format!("ID: {}", page_id), CONTENT_CY + 10, NEON_CYAN);
}

/// Successful enrollment.
pub fn show_enroll_ok<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, slot: u16) {
    clear_content(d);
    update_topbar(d, sb);
    draw_center(d, "ENROLLED", CONTENT_CY - 20, NEON_GREEN);
    draw_center(d, &format!("SLOT {}", slot), CONTENT_CY + 10, NEON_CYAN);
}

/// Factory-reset confirmation screen.
pub fn show_reset_ok<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>) {
    clear_content(d);
    update_topbar(d, sb);
    draw_center(d, "RESET OK", CONTENT_CY - 20, NEON_GREEN);
    draw_center(d, "REBOOTING...", CONTENT_CY + 10, NEON_CYAN);
}

/// No fingerprint match.
pub fn show_no_match<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>) {
    clear_content(d);
    update_topbar(d, sb);
    draw_center(d, "NO MATCH", CONTENT_CY - 10, NEON_RED);
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
    draw_center(d, ">> TOTP <<", CONTENT_CY - 40, NEON_CYAN);
    let code_str = format!("{:03} {:03}", code / 1000, code % 1000);
    draw_large_center(d, &code_str, CONTENT_CY - 5, NEON_GREEN);
}
