use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::error::TotpError;

type HmacSha1 = Hmac<Sha1>;

/// Decodes a base32-encoded string (RFC 4648) into `out`, returning the byte count.
///
/// - Case-insensitive: both upper and lower-case letters are accepted.
/// - Padding (`=`) is optional and silently skipped when present.
/// - Returns [`TotpError::SecretTooLong`] if `input.len() > 64`.
/// - Returns [`TotpError::InvalidBase32`] on any character outside the base32 alphabet.
fn base32_decode(input: &str, out: &mut [u8; 40]) -> Result<usize, TotpError> {
    if input.len() > 64 {
        return Err(TotpError::SecretTooLong);
    }

    let mut buffer: u32 = 0;
    let mut bits_left: u32 = 0;
    let mut out_idx = 0usize;

    for &byte in input.as_bytes() {
        if byte == b'=' {
            break; // optional padding — stop decoding
        }

        let val: u32 = match byte {
            b'A'..=b'Z' => (byte - b'A') as u32,
            b'a'..=b'z' => (byte - b'a') as u32,
            b'2'..=b'7' => (byte - b'2' + 26) as u32,
            _ => return Err(TotpError::InvalidBase32),
        };

        buffer = (buffer << 5) | val;
        bits_left += 5;

        if bits_left >= 8 {
            bits_left -= 8;
            // Safety: 64 base32 chars decode to at most 40 bytes — within bounds.
            out[out_idx] = ((buffer >> bits_left) & 0xFF) as u8;
            out_idx += 1;
        }
    }

    Ok(out_idx)
}

/// Returns a 6-digit TOTP code (RFC 6238 / RFC 4226, HMAC-SHA1, 30-second step).
///
/// - `secret_b32`: base32-encoded TOTP secret (case-insensitive, padding optional).
///   Maximum 64 characters, encoding up to 40 raw secret bytes.
/// - `timestamp`: Unix timestamp in seconds, provided by the device RTC or USB clock sync.
///   The 30-second window division is performed inside this function.
///
/// # Errors
///
/// - [`TotpError::InvalidBase32`] — `secret_b32` contains a character outside `A-Z`/`2-7`.
/// - [`TotpError::SecretTooLong`] — `secret_b32` is longer than 64 characters.
pub fn generate_totp(secret_b32: &str, timestamp: u64) -> Result<u32, TotpError> {
    // Step 1: decode the base32 secret into a fixed on-stack buffer.
    let mut secret_bytes = [0u8; 40];
    let secret_len = base32_decode(secret_b32, &mut secret_bytes)?;

    // Step 2: build the HOTP counter as an 8-byte big-endian integer.
    //         T = floor(Unix time / 30-second step), per RFC 6238 §4.
    let counter: u64 = timestamp / 30;
    let counter_bytes: [u8; 8] = counter.to_be_bytes();

    // Step 3: HMAC-SHA1(key = raw_secret, message = counter_bytes).
    let mut mac = HmacSha1::new_from_slice(&secret_bytes[..secret_len])
        // HMAC accepts any key length — this Err branch is unreachable in practice.
        .map_err(|_| TotpError::InvalidBase32)?;
    mac.update(&counter_bytes);
    let result = mac.finalize().into_bytes();

    // Step 4: dynamic truncation (RFC 4226 §5.3).
    //         offset = low-order nibble of the last HMAC byte.
    let offset = (result[19] & 0x0f) as usize;
    let code = u32::from_be_bytes([
        result[offset] & 0x7f, // clear the MSB (sign bit) per spec
        result[offset + 1],
        result[offset + 2],
        result[offset + 3],
    ]) % 1_000_000;

    Ok(code)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TotpError;

    // ── TOTP correctness ──────────────────────────────────────────────────────

    #[test]
    fn totp_known_vector_t59() {
        // "JBSWY3DPEHPK3PXP" decodes to b"Hello!\xDE\xAD\xBE\xEF" (10 raw bytes).
        // counter = floor(59 / 30) = 1.
        // 996554 is the correct 6-digit TOTP for this key/counter pair.
        assert_eq!(generate_totp("JBSWY3DPEHPK3PXP", 59).unwrap(), 996554);
    }

    #[test]
    fn totp_rfc6238_vector_t59() {
        // RFC 6238 Appendix B uses the ASCII key "12345678901234567890", whose base32
        // encoding is "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".
        // At t=59 the RFC specifies an 8-digit code of 94287082.
        // Our 6-digit implementation computes that value mod 10^6 = 287082.
        assert_eq!(
            generate_totp("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59).unwrap(),
            287082,
        );
    }

    #[test]
    fn totp_window_stability() {
        // Every timestamp within the same 30-second window must produce the same code.
        // The immediately following window must produce a different code.
        //
        // Window 2 spans [60, 89]  → counter = 2
        // Window 3 spans [90, 119] → counter = 3
        let code_w2 = generate_totp("JBSWY3DPEHPK3PXP", 60).unwrap();
        assert_eq!(generate_totp("JBSWY3DPEHPK3PXP", 60).unwrap(), code_w2); // window start
        assert_eq!(generate_totp("JBSWY3DPEHPK3PXP", 89).unwrap(), code_w2); // window end
        assert_ne!(generate_totp("JBSWY3DPEHPK3PXP", 90).unwrap(), code_w2); // next window
    }

    #[test]
    fn totp_case_insensitive_secret() {
        // The base32 alphabet is case-insensitive per RFC 4648 §3.3.
        // Upper and lower-case representations of the same secret must produce
        // identical codes.
        let upper = generate_totp("JBSWY3DPEHPK3PXP", 59).unwrap();
        let lower = generate_totp("jbswy3dpehpk3pxp", 59).unwrap();
        assert_eq!(upper, lower);
    }

    #[test]
    fn totp_padding_ignored() {
        // RFC 4648 base32 padding ('=') must be accepted and silently skipped.
        // "JBSWY3DPEHPK3PXP" zero-pads to "JBSWY3DPEHPK3PXP======" (6 padding chars).
        // The result must be identical to the unpadded form (996554).
        assert_eq!(generate_totp("JBSWY3DPEHPK3PXP======", 59).unwrap(), 996554);
    }

    // ── Error cases ───────────────────────────────────────────────────────────

    #[test]
    fn totp_invalid_base32_returns_err() {
        // Characters outside A-Z and 2-7 must be rejected immediately.
        assert_eq!(
            generate_totp("!!!INVALID!!!", 59),
            Err(TotpError::InvalidBase32),
        );
    }

    #[test]
    fn totp_digit_zero_and_one_are_invalid() {
        // '0' and '1' are not part of the base32 alphabet (RFC 4648 §6).
        // They are often confused with 'O' and 'I' but must not be accepted.
        assert_eq!(
            generate_totp("JBSWY0DPEHPK3PXP", 59),
            Err(TotpError::InvalidBase32)
        );
        assert_eq!(
            generate_totp("JBSWY1DPEHPK3PXP", 59),
            Err(TotpError::InvalidBase32)
        );
    }

    #[test]
    fn totp_secret_too_long_returns_err() {
        // 65 base32 characters is strictly more than the 64-char limit.
        // The early guard must fire before any decoding begins.
        let long = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(long.len(), 65, "test string must be exactly 65 chars");
        assert_eq!(generate_totp(long, 59), Err(TotpError::SecretTooLong));
    }

    #[test]
    fn totp_exactly_64_char_secret_accepted() {
        // 64 base32 characters decode to exactly 40 bytes — the buffer limit.
        // This must succeed without any buffer overflow or SecretTooLong error.
        let max_len = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(max_len.len(), 64, "test string must be exactly 64 chars");
        assert!(generate_totp(max_len, 59).is_ok());
    }

    // ── base32_decode internals ───────────────────────────────────────────────

    #[test]
    fn base32_decode_hello_deadbeef() {
        // "JBSWY3DPEHPK3PXP" must decode to b"Hello!\xDE\xAD\xBE\xEF".
        let mut out = [0u8; 40];
        let n = base32_decode("JBSWY3DPEHPK3PXP", &mut out).unwrap();
        assert_eq!(n, 10);
        assert_eq!(&out[..n], b"Hello!\xDE\xAD\xBE\xEF");
    }

    #[test]
    fn base32_decode_empty_string() {
        // An empty secret produces 0 decoded bytes without error.
        let mut out = [0u8; 40];
        let n = base32_decode("", &mut out).unwrap();
        assert_eq!(n, 0);
    }
}
