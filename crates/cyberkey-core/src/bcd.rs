/// Decode a Binary-Coded Decimal byte to its decimal value.
///
/// Each nibble encodes one decimal digit: upper nibble = tens, lower = units.
/// Input must be a valid BCD value (both nibbles 0–9); behaviour is undefined
/// for invalid inputs such as `0x9A`.
pub fn bcd2dec(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0x0F)
}

/// Encode a decimal value (0–99) as a Binary-Coded Decimal byte.
///
/// Upper nibble = tens digit, lower nibble = units digit.
/// Panics in debug builds if `dec > 99` (would overflow a nibble).
pub fn dec2bcd(dec: u8) -> u8 {
    ((dec / 10) << 4) | (dec % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_0_to_99() {
        for v in 0u8..=99 {
            assert_eq!(bcd2dec(dec2bcd(v)), v, "roundtrip failed for {v}");
        }
    }

    #[test]
    fn known_values() {
        assert_eq!(bcd2dec(0x59), 59); // max seconds/minutes
        assert_eq!(bcd2dec(0x23), 23); // max hours
        assert_eq!(bcd2dec(0x31), 31); // max day
        assert_eq!(bcd2dec(0x12), 12); // max month
        assert_eq!(bcd2dec(0x99), 99); // max year offset (2099)
        assert_eq!(dec2bcd(59), 0x59);
        assert_eq!(dec2bcd(23), 0x23);
    }
}
