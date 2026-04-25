//! Drawing helpers for the M5StickC Plus 2 display (ST7789V2).
//!
//! The panel is 135×240 pixels (portrait), driven with Rotation::Deg90
//! which produces a landscape logical frame of 240×135.

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_10X20, FONT_6X13},
        MonoTextStyle,
    },
    pixelcolor::Rgb565,
    prelude::*,
    text::{Alignment, Text, TextStyleBuilder},
};

/// Logical screen width after Deg270 rotation (pixels).
pub const W: u32 = 240;
/// Logical screen height after Deg270 rotation (pixels).
pub const H: u32 = 135;

/// Show a short status message centred on a black screen.
pub fn show_status<D: DrawTarget<Color = Rgb565>>(display: &mut D, msg: &str) {
    let _ = display.clear(Rgb565::BLACK);
    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let ts = TextStyleBuilder::new().alignment(Alignment::Center).build();
    let center = Point::new((W / 2) as i32, (H / 2) as i32);
    let _ = Text::with_text_style(msg, center, style, ts).draw(display);
}

/// Show a two-line status message centred on a black screen (smaller font).
pub fn show_status_2line<D: DrawTarget<Color = Rgb565>>(
    display: &mut D,
    line1: &str,
    line2: &str,
) {
    let _ = display.clear(Rgb565::BLACK);
    let style = MonoTextStyle::new(&FONT_6X13, Rgb565::WHITE);
    let ts = TextStyleBuilder::new().alignment(Alignment::Center).build();
    let cx = (W / 2) as i32;
    let cy = (H / 2) as i32;
    let _ = Text::with_text_style(line1, Point::new(cx, cy - 10), style, ts).draw(display);
    let _ = Text::with_text_style(line2, Point::new(cx, cy + 10), style, ts).draw(display);
}

/// Show a successful fingerprint match (green text).
pub fn show_auth_ok<D: DrawTarget<Color = Rgb565>>(display: &mut D, page_id: u16) {
    let _ = display.clear(Rgb565::BLACK);
    let ts = TextStyleBuilder::new().alignment(Alignment::Center).build();
    let cx = (W / 2) as i32;
    let cy = (H / 2) as i32;
    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN);
    let _ = Text::with_text_style("Auth OK", Point::new(cx, cy - 12), style, ts).draw(display);
    let id_str = format!("ID: {}", page_id);
    let id_style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let _ = Text::with_text_style(&id_str, Point::new(cx, cy + 12), id_style, ts).draw(display);
}

/// Show enrollment progress: "Place finger" + "N / total".
pub fn show_enroll_pass<D: DrawTarget<Color = Rgb565>>(display: &mut D, pass: u8, total: u8) {
    let _ = display.clear(Rgb565::BLACK);
    let ts = TextStyleBuilder::new().alignment(Alignment::Center).build();
    let cx = (W / 2) as i32;
    let cy = (H / 2) as i32;
    let label_style = MonoTextStyle::new(&FONT_6X13, Rgb565::WHITE);
    let _ = Text::with_text_style("Place finger", Point::new(cx, cy - 12), label_style, ts)
        .draw(display);
    let prog = format!("{} / {}", pass, total);
    let num_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_CYAN);
    let _ = Text::with_text_style(&prog, Point::new(cx, cy + 12), num_style, ts).draw(display);
}

/// Show enrollment success: "Enrolled" (green) + "Slot N".
pub fn show_enroll_ok<D: DrawTarget<Color = Rgb565>>(display: &mut D, slot: u16) {
    let _ = display.clear(Rgb565::BLACK);
    let ts = TextStyleBuilder::new().alignment(Alignment::Center).build();
    let cx = (W / 2) as i32;
    let cy = (H / 2) as i32;
    let ok_style = MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN);
    let _ = Text::with_text_style("Enrolled", Point::new(cx, cy - 12), ok_style, ts).draw(display);
    let slot_str = format!("Slot {}", slot);
    let slot_style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let _ = Text::with_text_style(&slot_str, Point::new(cx, cy + 12), slot_style, ts).draw(display);
}

/// Show a no-match result (red text).
pub fn show_no_match<D: DrawTarget<Color = Rgb565>>(display: &mut D) {
    let _ = display.clear(Rgb565::BLACK);
    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::RED);
    let ts = TextStyleBuilder::new().alignment(Alignment::Center).build();
    let _ = Text::with_text_style(
        "No match",
        Point::new((W / 2) as i32, (H / 2) as i32),
        style,
        ts,
    )
    .draw(display);
}

/// Show a TOTP code for 2 seconds (call before delay_ms(2000)).
pub fn show_totp<D: DrawTarget<Color = Rgb565>>(display: &mut D, code: u32) {
    let _ = display.clear(Rgb565::BLACK);
    let ts = TextStyleBuilder::new().alignment(Alignment::Center).build();
    let cx = (W / 2) as i32;
    let cy = (H / 2) as i32;
    let label_style = MonoTextStyle::new(&FONT_6X13, Rgb565::CSS_LIGHT_GRAY);
    let _ = Text::with_text_style("TOTP", Point::new(cx, cy - 20), label_style, ts).draw(display);
    let code_str = format!("{:03} {:03}", code / 1000, code % 1000);
    let code_style = MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN);
    let _ = Text::with_text_style(&code_str, Point::new(cx, cy + 10), code_style, ts).draw(display);
}

/// Show the 6-digit BLE passkey on a dark-blue background.
///
/// ```text
///   Bluetooth PIN
///     XXX XXX
/// ```
pub fn show_pin<D: DrawTarget<Color = Rgb565>>(display: &mut D, pin: u32) {
    let _ = display.clear(Rgb565::BLACK);

    let ts_center = TextStyleBuilder::new().alignment(Alignment::Center).build();
    let cx = (W / 2) as i32;
    let cy = (H / 2) as i32;

    let label_style = MonoTextStyle::new(&FONT_6X13, Rgb565::CSS_LIGHT_GRAY);
    let _ = Text::with_text_style(
        "Bluetooth PIN",
        Point::new(cx, cy - 20),
        label_style,
        ts_center,
    )
    .draw(display);

    let pin_str = format!("{:03} {:03}", pin / 1000, pin % 1000);
    let pin_style = MonoTextStyle::new(&FONT_10X20, Rgb565::YELLOW);
    let _ = Text::with_text_style(
        &pin_str,
        Point::new(cx, cy + 10),
        pin_style,
        ts_center,
    )
    .draw(display);
}
