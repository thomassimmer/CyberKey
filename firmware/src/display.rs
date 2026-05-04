//! Drawing helpers for the M5StickC Plus 2 display (ST7789V2).
//!
//! Logical frame: 240×135 landscape (after Rotation::Deg90).
//! Top 20 px: persistent status bar — time (yellow, left) + battery (cyan, right).
//! Content area: y = 20..135 (115 px tall).

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
};

use crate::fonts::orbitron_font::{draw_text_prop, get_text_width};
use crate::fonts::orbitron_large::{
    draw_text_prop as draw_large, get_text_width as get_large_width,
};
use crate::fonts::orbitron_mini::{draw_text_prop as draw_mini, get_text_width as get_mini_width};

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

/// Dark cyan for status bar background (simulates low-opacity NEON_CYAN).
const BAR_BG: Rgb565 = Rgb565::new(0, 4, 2);

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

fn draw_mini_center<D: DrawTarget<Color = Rgb565>>(d: &mut D, text: &str, y: i32, color: Rgb565) {
    let w = get_mini_width(text);
    let x = (W as i32 - w) / 2;
    let _ = draw_mini(d, text, Point::new(x, y), color);
}

/// Truncate `text` so that it fits within `max_px` pixels using the mini font.
/// Appends "..." if truncation was necessary.
fn truncate_to_fit(text: &str, max_px: i32) -> std::borrow::Cow<'_, str> {
    if get_mini_width(text) <= max_px {
        return std::borrow::Cow::Borrowed(text);
    }
    let ellipsis = "...";
    let ellipsis_w = get_mini_width(ellipsis);
    let mut end = 0;
    for (i, _) in text.char_indices() {
        let candidate = &text[..i];
        if get_mini_width(candidate) + ellipsis_w > max_px {
            break;
        }
        end = i;
    }
    std::borrow::Cow::Owned(format!("{}{}", &text[..end], ellipsis))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Redraw only the top status bar — called on every state transition and when
/// the minute changes, without touching the content area.
pub fn update_topbar<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>) {
    // Background
    let _ = Rectangle::new(Point::new(0, 0), Size::new(W, BAR_H))
        .into_styled(PrimitiveStyle::with_fill(BAR_BG))
        .draw(d);

    // Time — yellow, left-aligned;
    let _ = draw_mini(d, sb.time, Point::new(5, 7), NEON_CYAN);

    // Battery — cyan (red when ≤ 20 %, dim when unknown), right-aligned.
    let (bat_str, bat_color) = match sb.battery {
        Some(pct) => (
            format!("{}%", pct),
            if pct <= 20 { NEON_RED } else { NEON_CYAN },
        ),
        None => ("--%".to_string(), NEON_CYAN),
    };
    draw_mini_right(d, &bat_str, 7, bat_color);

    // Separator line
    let _ = Line::new(
        Point::new(0, BAR_H as i32 - 1),
        Point::new(W as i32 - 1, BAR_H as i32 - 1),
    )
    .into_styled(PrimitiveStyle::with_stroke(NEON_YELLOW, 1))
    .draw(d);
}

/// Idle screen: BLE pairing PIN.
///
/// ```text
/// >> BT PAIRING <<
///    XXX XXX
///  enter on host
/// ```
pub fn show_pin<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    sb: &StatusBar<'_>,
    pin: u32,
    conn_count: u32,
) {
    clear_content(d);
    update_topbar(d, sb);
    draw_mini_center(d, ">> BT PAIRING <<", CONTENT_CY - 40, NEON_CYAN);

    let pin_str = format!("{:03} {:03}", pin / 1000, pin % 1000);
    draw_large_center(d, &pin_str, CONTENT_CY - 15, NEON_YELLOW);

    let status_str = format!("ACTIVE CLIENTS: {}", conn_count);
    draw_mini_center(d, &status_str, CONTENT_CY + 25, NEON_CYAN);
}

/// Single-line status message centred in content area.
pub fn show_status<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, msg: &str) {
    clear_content(d);
    update_topbar(d, sb);
    draw_mini_center(d, &msg.to_uppercase(), CONTENT_CY - 10, NEON_CYAN);
}

/// Power-off screen (large text).
pub fn show_power_off<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>) {
    clear_content(d);
    update_topbar(d, sb);
    draw_mini_center(d, "POWERING OFF...", CONTENT_CY - 10, NEON_RED);
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
    draw_mini_center(d, &line1.to_uppercase(), CONTENT_CY - 20, NEON_CYAN);
    draw_mini_center(d, &line2.to_uppercase(), CONTENT_CY + 10, NEON_CYAN);
}

/// Successful enrollment.
pub fn show_enroll_ok<D: DrawTarget<Color = Rgb565>>(d: &mut D, sb: &StatusBar<'_>, slot: u8) {
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

/// TOTP code display with service label.
///
/// ```text
///    GITHUB
///  XXX XXX
/// ```
pub fn show_totp<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    sb: &StatusBar<'_>,
    label: &str,
    code: u32,
) {
    const LABEL_MAX_PX: i32 = W as i32 - 20;
    clear_content(d);
    update_topbar(d, sb);
    let label_upper = label.to_uppercase();
    let label_fit = truncate_to_fit(&label_upper, LABEL_MAX_PX);
    draw_mini_center(d, &label_fit, CONTENT_CY - 35, NEON_CYAN);
    let code_str = format!("{:03} {:03}", code / 1000, code % 1000);
    draw_large_center(d, &code_str, CONTENT_CY - 10, NEON_GREEN);
}

/// Controls display controller power (SLPIN / SLPOUT).
///
/// Implemented by the concrete display type in `main.rs`.
/// `set_sleep_mode(true)` sends SLPIN; `set_sleep_mode(false)` sends SLPOUT and waits for the panel to wake.
pub trait DisplayPower {
    fn set_sleep_mode(&mut self, sleeping: bool);
}
