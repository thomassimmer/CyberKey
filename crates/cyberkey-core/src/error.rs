/// Errors that can arise when generating a TOTP code.
#[derive(Debug, Clone, PartialEq)]
pub enum TotpError {
    /// `secret_b32` contains a character outside the RFC 4648 base32 alphabet.
    InvalidBase32,
    /// Input exceeds 64 base32 characters — the decoded secret would overflow the
    /// 40-byte on-stack buffer.
    SecretTooLong,
}

/// Errors that can arise when constructing a [`TotpEntry`] or mutating a [`CyberKeyConfig`].
///
/// [`TotpEntry`]: crate::config::TotpEntry
/// [`CyberKeyConfig`]: crate::config::CyberKeyConfig
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    /// The config already holds 10 entries and cannot accept another.
    Full,
    /// Another entry already claims this `finger_id`; use `remove_by_label` first.
    DuplicateFingerSlot,
    /// The provided label exceeds 32 characters. The caller must truncate it.
    LabelTooLong,
    /// The provided `secret_b32` exceeds 64 characters.
    SecretTooLong,
    /// No entry matched the requested label.
    EntryNotFound,
}
