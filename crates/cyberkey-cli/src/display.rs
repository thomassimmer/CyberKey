//! `display` — table rendering and secret masking for CLI output.
//!
//! All functions are pure (no I/O) so they can be unit-tested without a
//! connected device.

use tabled::{Table, Tabled};

use crate::protocol::EntryInfo;

// ── Secret masking ────────────────────────────────────────────────────────────

/// Number of plaintext characters shown at the start of a masked secret.
const VISIBLE_PREFIX_LEN: usize = 4;

/// Number of `*` characters appended after the visible prefix.
const MASK_STARS: usize = 20;

/// Masks a TOTP secret for display.
///
/// Shows the first [`VISIBLE_PREFIX_LEN`] characters in plaintext, then appends
/// exactly [`MASK_STARS`] asterisk characters regardless of the actual secret
/// length. If the secret is shorter than [`VISIBLE_PREFIX_LEN`], all available
/// characters are shown.
///
/// # Examples
///
/// ```
/// # use cyberkey_cli::display::mask_secret;
/// assert_eq!(mask_secret("JBSWY3DPEHPK3PXP"), "JBSW********************");
/// assert_eq!(mask_secret("AB"),               "AB********************");
/// assert_eq!(mask_secret(""),                 "********************");
/// ```
pub fn mask_secret(secret: &str) -> String {
    let visible_len = secret.len().min(VISIBLE_PREFIX_LEN);
    let visible = &secret[..visible_len];
    format!("{}{}", visible, "*".repeat(MASK_STARS))
}

// ── Table rendering ───────────────────────────────────────────────────────────

/// Internal row type used by [`tabled`] for rendering.
#[derive(Tabled)]
struct EntryRow {
    #[tabled(rename = "Slot")]
    slot: u8,

    #[tabled(rename = "Service")]
    label: String,

    #[tabled(rename = "Secret (masked)")]
    secret_masked: String,
}

/// Renders a slice of [`EntryInfo`] values as a formatted ASCII table.
///
/// Returns a placeholder string when `entries` is empty rather than an empty
/// table, which would look confusing in the terminal.
///
/// The `secret_masked` field is displayed verbatim — it is the firmware's
/// already-masked representation of the secret.
pub fn render_entries_table(entries: &[EntryInfo]) -> String {
    if entries.is_empty() {
        return "  (no entries configured)".to_string();
    }

    let rows: Vec<EntryRow> = entries
        .iter()
        .map(|e| EntryRow {
            slot: e.slot,
            label: e.label.clone(),
            secret_masked: e.secret_masked.clone(),
        })
        .collect();

    Table::new(rows).to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── mask_secret ───────────────────────────────────────────────────────────

    /// Normal secret longer than 4 chars — only first 4 are visible.
    #[test]
    fn mask_secret_normal() {
        assert_eq!(mask_secret("JBSWY3DPEHPK3PXP"), "JBSW********************");
    }

    /// Secret with exactly 4 characters — entire secret is visible, then mask.
    #[test]
    fn mask_secret_exactly_prefix_length() {
        assert_eq!(mask_secret("JBSW"), "JBSW********************");
    }

    /// Secret shorter than the visible prefix — all chars shown, then mask.
    #[test]
    fn mask_secret_shorter_than_prefix() {
        assert_eq!(mask_secret("AB"), "AB********************");
    }

    /// Single character secret.
    #[test]
    fn mask_secret_single_char() {
        assert_eq!(mask_secret("X"), "X********************");
    }

    /// Empty secret — produces only the mask.
    #[test]
    fn mask_secret_empty() {
        assert_eq!(mask_secret(""), "********************");
    }

    /// The mask is always exactly MASK_STARS stars long regardless of input.
    #[test]
    fn mask_secret_star_count_is_always_20() {
        for input in &["", "A", "ABCD", "ABCDE", "JBSWY3DPEHPK3PXP"] {
            let result = mask_secret(input);
            let star_suffix: String = result.chars().filter(|&c| c == '*').collect();
            assert_eq!(
                star_suffix.len(),
                MASK_STARS,
                "wrong star count for input {input:?}"
            );
        }
    }

    /// The visible prefix is capped at VISIBLE_PREFIX_LEN even for very long secrets.
    #[test]
    fn mask_secret_prefix_capped_at_4() {
        let long = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // 64 A's
        let result = mask_secret(long);
        assert!(result.starts_with("AAAA"));
        // Exactly 4 visible chars + 20 stars = 24 total chars.
        assert_eq!(result.len(), VISIBLE_PREFIX_LEN + MASK_STARS);
    }

    // ── render_entries_table ──────────────────────────────────────────────────

    /// Empty slice produces the placeholder string.
    #[test]
    fn render_empty_entries() {
        let result = render_entries_table(&[]);
        assert!(
            result.contains("no entries"),
            "expected placeholder, got: {result:?}"
        );
    }

    /// A non-empty table contains all labels.
    #[test]
    fn render_table_contains_labels() {
        let entries = vec![
            EntryInfo {
                slot: 0,
                label: "GitHub".to_string(),
                secret_masked: "JBSW********************".to_string(),
            },
            EntryInfo {
                slot: 1,
                label: "AWS".to_string(),
                secret_masked: "OJA3********************".to_string(),
            },
        ];
        let table = render_entries_table(&entries);
        assert!(table.contains("GitHub"), "table missing 'GitHub'");
        assert!(table.contains("AWS"), "table missing 'AWS'");
    }

    /// Column headers must be present.
    #[test]
    fn render_table_has_headers() {
        let entries = vec![EntryInfo {
            slot: 0,
            label: "Test".to_string(),
            secret_masked: "TEST********************".to_string(),
        }];
        let table = render_entries_table(&entries);
        assert!(table.contains("Slot"), "table missing 'Slot' header");
        assert!(table.contains("Service"), "table missing 'Service' header");
        assert!(table.contains("Secret"), "table missing 'Secret' header");
    }
}
