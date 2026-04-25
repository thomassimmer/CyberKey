//! HID keyboard constants and keystroke helpers.
//!
//! Pure `no_std` logic — no hardware dependencies.
//! Used by the firmware and fully unit-testable on the host.

#![cfg_attr(not(test), no_std)]

const SHIFT: u8 = 0x80;

/// USB HID keycodes for ASCII bytes 0x00–0x7E.
/// Each entry is a raw keycode, or `keycode | SHIFT` for shifted keys.
pub const ASCII_MAP: &[u8] = &[
    // 0x00–0x1F control chars
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // NUL SOH STX ETX EOT ENQ ACK BEL
    0x2a, 0x2b, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, // BS  TAB LF  VT  FF  CR  SO  SI
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // DEL DC1 DC2 DC3 DC4 NAK SYN ETB
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // CAN EM  SUB ESC FS  GS  RS  US
    // 0x20–0x2F printable
    0x2c,         // ' '
    0x1e | SHIFT, // !
    0x34 | SHIFT, // "
    0x20 | SHIFT, // #
    0x21 | SHIFT, // $
    0x22 | SHIFT, // %
    0x24 | SHIFT, // &
    0x34,         // '
    0x26 | SHIFT, // (
    0x27 | SHIFT, // )
    0x25 | SHIFT, // *
    0x2e | SHIFT, // +
    0x36,         // ,
    0x2d,         // -
    0x37,         // .
    0x38,         // /
    // 0x30–0x39 digits
    0x27, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26,
    // 0x3A–0x40
    0x33 | SHIFT, // :
    0x33,         // ;
    0x36 | SHIFT, // <
    0x2e,         // =
    0x37 | SHIFT, // >
    0x38 | SHIFT, // ?
    0x1f | SHIFT, // @
    // 0x41–0x5A uppercase A–Z
    0x04 | SHIFT, 0x05 | SHIFT, 0x06 | SHIFT, 0x07 | SHIFT, 0x08 | SHIFT,
    0x09 | SHIFT, 0x0a | SHIFT, 0x0b | SHIFT, 0x0c | SHIFT, 0x0d | SHIFT,
    0x0e | SHIFT, 0x0f | SHIFT, 0x10 | SHIFT, 0x11 | SHIFT, 0x12 | SHIFT,
    0x13 | SHIFT, 0x14 | SHIFT, 0x15 | SHIFT, 0x16 | SHIFT, 0x17 | SHIFT,
    0x18 | SHIFT, 0x19 | SHIFT, 0x1a | SHIFT, 0x1b | SHIFT, 0x1c | SHIFT,
    0x1d | SHIFT,
    // 0x5B–0x60
    0x2f,         // [
    0x31,         // backslash
    0x30,         // ]
    0x23 | SHIFT, // ^
    0x2d | SHIFT, // _
    0x35,         // `
    // 0x61–0x7A lowercase a–z
    0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
    0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
    // 0x7B–0x7E
    0x2f | SHIFT, // {
    0x31 | SHIFT, // |
    0x30 | SHIFT, // }
    0x35 | SHIFT, // ~
    0x00,         // DEL
];

/// Translate a printable ASCII byte to `(modifier_byte, hid_keycode)`.
/// Returns `(0, 0)` for unmapped or out-of-range characters.
pub fn ascii_to_key(c: u8) -> (u8, u8) {
    let idx = c as usize;
    if idx >= ASCII_MAP.len() {
        return (0, 0);
    }
    let v = ASCII_MAP[idx];
    if v == 0 {
        return (0, 0);
    }
    if v & SHIFT != 0 {
        (0x02, v & !SHIFT) // Left Shift modifier + keycode
    } else {
        (0x00, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- lowercase letters ---

    #[test]
    fn lowercase_a_has_no_modifier() {
        assert_eq!(ascii_to_key(b'a'), (0x00, 0x04));
    }

    #[test]
    fn lowercase_z_has_no_modifier() {
        assert_eq!(ascii_to_key(b'z'), (0x00, 0x1d));
    }

    #[test]
    fn all_lowercase_letters_have_no_modifier() {
        for c in b'a'..=b'z' {
            let (modifier, keycode) = ascii_to_key(c);
            assert_eq!(modifier, 0x00, "modifier should be 0 for '{}'", c as char);
            assert_ne!(keycode, 0x00, "keycode should be non-zero for '{}'", c as char);
        }
    }

    // --- uppercase letters ---

    #[test]
    fn uppercase_a_uses_left_shift() {
        assert_eq!(ascii_to_key(b'A'), (0x02, 0x04));
    }

    #[test]
    fn uppercase_z_uses_left_shift() {
        assert_eq!(ascii_to_key(b'Z'), (0x02, 0x1d));
    }

    #[test]
    fn all_uppercase_letters_share_keycode_with_lowercase() {
        for offset in 0u8..26 {
            let lower = b'a' + offset;
            let upper = b'A' + offset;
            let (lo_mod, lo_key) = ascii_to_key(lower);
            let (up_mod, up_key) = ascii_to_key(upper);
            assert_eq!(lo_key, up_key, "keycode mismatch for '{}'/'{}'", lower as char, upper as char);
            assert_eq!(lo_mod, 0x00);
            assert_eq!(up_mod, 0x02);
        }
    }

    // --- digits ---

    #[test]
    fn digit_1_no_modifier() {
        assert_eq!(ascii_to_key(b'1'), (0x00, 0x1e));
    }

    #[test]
    fn digit_0_no_modifier() {
        // '0' is at the END of the row on US layout → keycode 0x27
        assert_eq!(ascii_to_key(b'0'), (0x00, 0x27));
    }

    #[test]
    fn all_digits_have_no_modifier() {
        for c in b'0'..=b'9' {
            let (modifier, keycode) = ascii_to_key(c);
            assert_eq!(modifier, 0x00, "modifier should be 0 for '{}'", c as char);
            assert_ne!(keycode, 0x00, "keycode should be non-zero for '{}'", c as char);
        }
    }

    // --- shifted symbols (share keycode with digit row) ---

    #[test]
    fn exclamation_is_shifted_1() {
        let (_, key_1) = ascii_to_key(b'1');
        let (mod_bang, key_bang) = ascii_to_key(b'!');
        assert_eq!(key_bang, key_1, "'!' and '1' must share keycode");
        assert_eq!(mod_bang, 0x02, "'!' requires left-shift");
    }

    #[test]
    fn at_sign_is_shifted_2() {
        let (_, key_2) = ascii_to_key(b'2');
        let (mod_at, key_at) = ascii_to_key(b'@');
        assert_eq!(key_at, key_2);
        assert_eq!(mod_at, 0x02);
    }

    // --- common unshifted symbols ---

    #[test]
    fn space_maps_to_0x2c() {
        assert_eq!(ascii_to_key(b' '), (0x00, 0x2c));
    }

    #[test]
    fn period_no_modifier() {
        assert_eq!(ascii_to_key(b'.'), (0x00, 0x37));
    }

    #[test]
    fn hyphen_no_modifier() {
        assert_eq!(ascii_to_key(b'-'), (0x00, 0x2d));
    }

    #[test]
    fn underscore_is_shifted_hyphen() {
        let (_, key_minus) = ascii_to_key(b'-');
        let (mod_under, key_under) = ascii_to_key(b'_');
        assert_eq!(key_under, key_minus);
        assert_eq!(mod_under, 0x02);
    }

    // --- whitespace / control ---

    #[test]
    fn newline_maps_to_enter() {
        // LF (0x0A) → HID Enter (0x28)
        assert_eq!(ascii_to_key(b'\n'), (0x00, 0x28));
    }

    #[test]
    fn tab_maps_to_tab_key() {
        assert_eq!(ascii_to_key(b'\t'), (0x00, 0x2b));
    }

    #[test]
    fn backspace_maps_to_backspace_key() {
        assert_eq!(ascii_to_key(b'\x08'), (0x00, 0x2a));
    }

    #[test]
    fn null_byte_returns_no_key() {
        assert_eq!(ascii_to_key(0x00), (0, 0));
    }

    #[test]
    fn escape_returns_no_key() {
        assert_eq!(ascii_to_key(0x1b), (0, 0));
    }

    // --- out-of-range ---

    #[test]
    fn byte_above_0x7e_returns_no_key() {
        assert_eq!(ascii_to_key(0x80), (0, 0));
        assert_eq!(ascii_to_key(0xFF), (0, 0));
    }

    // --- integration: "Hello!" encodes to correct sequence ---

    #[test]
    fn hello_bang_encodes_correctly() {
        // 'H' → shift + h keycode
        assert_eq!(ascii_to_key(b'H'), (0x02, 0x0b));
        // 'e' → no shift
        assert_eq!(ascii_to_key(b'e'), (0x00, 0x08));
        // 'l' → no shift
        assert_eq!(ascii_to_key(b'l'), (0x00, 0x0f));
        // 'o' → no shift
        assert_eq!(ascii_to_key(b'o'), (0x00, 0x12));
        // '!' → shift + 1 keycode
        assert_eq!(ascii_to_key(b'!'), (0x02, 0x1e));
    }
}
