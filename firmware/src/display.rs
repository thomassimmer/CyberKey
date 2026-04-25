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
