/// Errors that can arise when generating a TOTP code.
#[derive(Debug, Clone, PartialEq)]
pub enum TotpError {
    /// `secret_b32` contains a character outside the RFC 4648 base32 alphabet.
    InvalidBase32,
    /// Input exceeds 64 base32 characters — the decoded secret would overflow the
    /// 40-byte on-stack buffer.
    SecretTooLong,
}
