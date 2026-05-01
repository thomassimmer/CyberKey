//! Opcode constants and typed parameter types for the Fingerprint2 protocol.
//!
//! Opcode names are preserved verbatim from the M5Stack STM32 firmware source
//! so that `grep PS_AutoEnroll` maps directly between the C++ reference and
//! this Rust driver.

// ---------------------------------------------------------------------------
// Opcode constants
// ---------------------------------------------------------------------------

/// Collect a finger image into the image buffer (identification / smart-poll use).
pub const PS_GET_IMAGE: u8 = 0x01;
/// Collect a finger image optimised for enrollment quality.
///
/// Functionally identical to [`PS_GET_IMAGE`] at the protocol level (no parameters,
/// same ACK format), but the sensor firmware may apply different internal thresholds.
/// Always prefer this opcode during enrollment passes; use [`PS_GET_IMAGE`] for
/// identification polling.
pub const PS_GET_ENROLL_IMAGE: u8 = 0x29;
/// Generate a character file from the image buffer into a char buffer slot.
pub const PS_GEN_CHAR: u8 = 0x02;
/// Search the library for a matching template.
pub const PS_SEARCH: u8 = 0x04;
/// Combine two char buffers into a template.
pub const PS_REG_MODEL: u8 = 0x05;
/// Store a template from a char buffer into flash at a given page ID.
pub const PS_STORE_CHAR: u8 = 0x06;
/// Delete one or more stored templates.
///
/// Note: the upstream typo (`DeletChar`, not `DeleteChar`) is preserved for
/// traceability with the M5Stack C++ source.
pub const PS_DELET_CHAR: u8 = 0x0C;
/// Empty the entire template library.
pub const PS_EMPTY: u8 = 0x0D;
/// High-level autonomous identification — returns the matched page ID.
pub const PS_AUTO_IDENTIFY: u8 = 0x32;
/// Verify that the module is powered on and responsive.
pub const PS_HANDSHAKE: u8 = 0x35;
/// Set the operating mode of the sensor.
pub const PS_SET_WORK_MODE: u8 = 0xD2;
/// Set the sleep timeout (10-254 seconds).
pub const PS_SET_SLEEP_TIME: u8 = 0xD0;
/// Control the RGB LED ring.
pub const PS_CONTROL_BLN: u8 = 0x3C;
/// Activate the sensor (required after cold boot on some firmware versions).
pub const PS_ACTIVATE: u8 = 0xD4;

// ---------------------------------------------------------------------------
// LedMode
// ---------------------------------------------------------------------------

/// Selects the animation style for the RGB LED ring.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LedMode {
    /// LED fades in and out repeatedly.
    Breathing = 1,
    /// LED blinks on and off sharply.
    Flashing = 2,
    /// LED stays on continuously.
    On = 3,
    /// LED stays off.
    Off = 4,
    /// LED fades from off to full brightness once.
    FadeIn = 5,
    /// LED fades from full brightness to off once.
    FadeOut = 6,
}

// ---------------------------------------------------------------------------
// LedColor
// ---------------------------------------------------------------------------

/// Selects the colour of the RGB LED ring.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LedColor {
    Off = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Purple = 5,
    Yellow = 6,
    White = 7,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::is_wakeup_packet;

    // ------------------------------------------------------------------
    // LedColor repr values
    // ------------------------------------------------------------------

    /// Sentinel check: these values come directly from the protocol datasheet.
    /// A wrong repr would silently mis-program the LED ring on hardware.
    #[test]
    fn led_color_repr_values() {
        assert_eq!(LedColor::Off as u8, 0);
        assert_eq!(LedColor::Blue as u8, 1);
        assert_eq!(LedColor::Green as u8, 2);
        assert_eq!(LedColor::Cyan as u8, 3);
        assert_eq!(LedColor::Red as u8, 4);
        assert_eq!(LedColor::Purple as u8, 5);
        assert_eq!(LedColor::Yellow as u8, 6);
        assert_eq!(LedColor::White as u8, 7);
    }

    // ------------------------------------------------------------------
    // LedMode repr values
    // ------------------------------------------------------------------

    #[test]
    fn led_mode_repr_values() {
        assert_eq!(LedMode::Breathing as u8, 1);
        assert_eq!(LedMode::Flashing as u8, 2);
        assert_eq!(LedMode::On as u8, 3);
        assert_eq!(LedMode::Off as u8, 4);
        assert_eq!(LedMode::FadeIn as u8, 5);
        assert_eq!(LedMode::FadeOut as u8, 6);
    }

    // ------------------------------------------------------------------
    // is_wakeup_packet (cross-module sanity checks)
    // ------------------------------------------------------------------

    /// The exact wakeup sequence must be recognised.
    #[test]
    fn is_wakeup_packet_true() {
        let wakeup = [
            0xEF_u8, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x07, 0x00, 0x03, 0xFF, 0x01, 0x09,
        ];
        assert!(is_wakeup_packet(&wakeup));
    }

    /// A successful handshake ACK is not a wakeup packet.
    #[test]
    fn is_wakeup_packet_false_on_handshake_ack() {
        // TYPE=0x07, LEN=0x0003, DATA=[0x00], CSUM=(7+3+0)=10=0x000A
        let ack = [
            0xEF_u8, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x07, 0x00, 0x03, 0x00, 0x00, 0x0A,
        ];
        assert!(!is_wakeup_packet(&ack));
    }

    /// A short slice must return false without panicking.
    #[test]
    fn is_wakeup_packet_false_on_short_slice() {
        assert!(!is_wakeup_packet(&[0xEF, 0x01, 0xFF]));
    }
}
