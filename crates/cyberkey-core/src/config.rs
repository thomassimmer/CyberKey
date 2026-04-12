use heapless::{String, Vec};

use crate::error::ConfigError;

/// A single configured TOTP service, bound to one fingerprint sensor slot.
#[derive(Debug, Clone, PartialEq)]
pub struct TotpEntry {
    /// Fingerprint sensor slot index (0–9). Must be unique within a [`CyberKeyConfig`].
    pub finger_id: u8,
    /// Human-readable service name, e.g. `"GitHub"` or `"AWS"`. Max 32 characters.
    pub label: String<32>,
    /// Base32-encoded TOTP secret as provisioned by the service. Max 64 characters.
    /// Stored in undecoded form; decoded on-demand inside `generate_totp`.
    pub secret_b32: String<64>,
}

impl TotpEntry {
    /// Constructs a [`TotpEntry`] from plain string slices, validating field lengths.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::LabelTooLong`]   — `label.len() > 32`
    /// - [`ConfigError::SecretTooLong`]  — `secret_b32.len() > 64`
    pub fn new(finger_id: u8, label: &str, secret_b32: &str) -> Result<Self, ConfigError> {
        let label_hs: String<32> =
            String::try_from(label).map_err(|_| ConfigError::LabelTooLong)?;
        let secret_hs: String<64> =
            String::try_from(secret_b32).map_err(|_| ConfigError::SecretTooLong)?;
        Ok(Self {
            finger_id,
            label: label_hs,
            secret_b32: secret_hs,
        })
    }
}

/// In-memory TOTP configuration — capped at 10 entries (one per fingerprint slot).
///
/// All storage is stack-allocated via [`heapless`]; no heap allocator is required.
/// Entries are kept in insertion order.
///
/// # Behavioural guarantees (locked in by tests)
///
/// - **Duplicate `finger_id`** → [`ConfigError::DuplicateFingerSlot`]. A slot
///   reassignment requires an explicit [`remove_by_label`] + [`add_entry`] round-trip;
///   silent overwrites are never acceptable on a security device.
/// - **Label > 32 chars** → [`ConfigError::LabelTooLong`] (from [`TotpEntry::new`]).
///   The CLI layer is responsible for truncating; the core never silently drops characters.
/// - **Secret > 64 chars** → [`ConfigError::SecretTooLong`] (from [`TotpEntry::new`]).
///
/// [`remove_by_label`]: CyberKeyConfig::remove_by_label
/// [`add_entry`]: CyberKeyConfig::add_entry
#[derive(Debug, Clone)]
pub struct CyberKeyConfig {
    /// The list of configured entries, in insertion order.
    pub entries: Vec<TotpEntry, 10>,
}

impl CyberKeyConfig {
    /// Creates an empty config.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Adds a new entry to the config.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::DuplicateFingerSlot`] — `entry.finger_id` is already claimed
    ///   by an existing entry.
    /// - [`ConfigError::Full`] — the config already holds 10 entries.
    pub fn add_entry(&mut self, entry: TotpEntry) -> Result<(), ConfigError> {
        if self.entries.iter().any(|e| e.finger_id == entry.finger_id) {
            return Err(ConfigError::DuplicateFingerSlot);
        }
        // `push` returns the item back on overflow; discard it and map to ConfigError.
        self.entries.push(entry).map_err(|_| ConfigError::Full)
    }

    /// Removes the first entry whose label exactly matches `label` (case-sensitive).
    ///
    /// Insertion order of all remaining entries is preserved.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::EntryNotFound`] — no entry with the given label exists.
    pub fn remove_by_label(&mut self, label: &str) -> Result<(), ConfigError> {
        let pos = self
            .entries
            .iter()
            .position(|e| e.label.as_str() == label)
            .ok_or(ConfigError::EntryNotFound)?;
        self.entries.remove(pos);
        Ok(())
    }

    /// Returns a reference to the first entry with the given `finger_id`, or `None`.
    ///
    /// O(n) linear scan — n ≤ 10, so effectively constant time in practice.
    pub fn find_by_finger_id(&self, id: u8) -> Option<&TotpEntry> {
        self.entries.iter().find(|e| e.finger_id == id)
    }

    /// Iterates over all entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &TotpEntry> {
        self.entries.iter()
    }
}

impl Default for CyberKeyConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ConfigError;

    const SECRET: &str = "JBSWY3DPEHPK3PXP";

    /// Helper: build a valid entry for a given finger slot.
    fn make_entry(finger_id: u8) -> TotpEntry {
        TotpEntry::new(finger_id, "label", SECRET).unwrap()
    }

    // ── TotpEntry::new ────────────────────────────────────────────────────────

    #[test]
    fn entry_new_valid() {
        let e = TotpEntry::new(0, "GitHub", SECRET);
        assert!(e.is_ok());
        let e = e.unwrap();
        assert_eq!(e.finger_id, 0);
        assert_eq!(e.label.as_str(), "GitHub");
        assert_eq!(e.secret_b32.as_str(), SECRET);
    }

    #[test]
    fn entry_new_label_boundary_32_accepted() {
        // Exactly 32 characters must be accepted.
        let ok = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // 32 A's
        assert_eq!(ok.len(), 32);
        assert!(TotpEntry::new(0, ok, SECRET).is_ok());
    }

    #[test]
    fn entry_new_label_boundary_33_rejected() {
        // 33 characters must be rejected — never silently truncated.
        let bad = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // 33 A's
        assert_eq!(bad.len(), 33);
        assert_eq!(
            TotpEntry::new(0, bad, SECRET),
            Err(ConfigError::LabelTooLong),
        );
    }

    #[test]
    fn entry_new_secret_boundary_64_accepted() {
        // Exactly 64 base32 characters must be accepted.
        let ok = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // 64 A's
        assert_eq!(ok.len(), 64);
        assert!(TotpEntry::new(0, "svc", ok).is_ok());
    }

    #[test]
    fn entry_new_secret_boundary_65_rejected() {
        // 65 characters must be rejected.
        let bad = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // 65 A's
        assert_eq!(bad.len(), 65);
        assert_eq!(
            TotpEntry::new(0, "svc", bad),
            Err(ConfigError::SecretTooLong),
        );
    }

    // ── CyberKeyConfig::add_entry ─────────────────────────────────────────────

    #[test]
    fn config_add_and_find() {
        let mut cfg = CyberKeyConfig::new();
        cfg.add_entry(TotpEntry::new(2, "GitHub", SECRET).unwrap())
            .unwrap();
        let found = cfg.find_by_finger_id(2).unwrap();
        assert_eq!(found.label.as_str(), "GitHub");
    }

    #[test]
    fn config_capacity_at_limit() {
        let mut cfg = CyberKeyConfig::new();
        for i in 0u8..10 {
            cfg.add_entry(make_entry(i)).unwrap();
        }
        // finger_id 10 is new (not a duplicate), but the Vec is full.
        assert_eq!(
            cfg.add_entry(TotpEntry::new(10, "overflow", SECRET).unwrap()),
            Err(ConfigError::Full),
        );
    }

    #[test]
    fn config_duplicate_finger_slot_rejected() {
        let mut cfg = CyberKeyConfig::new();
        cfg.add_entry(TotpEntry::new(0, "GitHub", SECRET).unwrap())
            .unwrap();
        // Same finger_id, different label — must still be rejected.
        assert_eq!(
            cfg.add_entry(TotpEntry::new(0, "AWS", SECRET).unwrap()),
            Err(ConfigError::DuplicateFingerSlot),
        );
    }

    #[test]
    fn config_duplicate_check_precedes_capacity_check() {
        // If the config is full AND the new entry would be a duplicate,
        // DuplicateFingerSlot is reported (checked first).
        let mut cfg = CyberKeyConfig::new();
        for i in 0u8..10 {
            cfg.add_entry(make_entry(i)).unwrap();
        }
        // finger_id 0 is both a duplicate and the config is full.
        assert_eq!(
            cfg.add_entry(TotpEntry::new(0, "extra", SECRET).unwrap()),
            Err(ConfigError::DuplicateFingerSlot),
        );
    }

    // ── CyberKeyConfig::remove_by_label ───────────────────────────────────────

    #[test]
    fn config_remove_happy_path() {
        let mut cfg = CyberKeyConfig::new();
        cfg.add_entry(TotpEntry::new(0, "GitHub", SECRET).unwrap())
            .unwrap();
        assert!(cfg.remove_by_label("GitHub").is_ok());
        assert!(cfg.entries.is_empty());
    }

    #[test]
    fn config_remove_not_found() {
        let mut cfg = CyberKeyConfig::new();
        assert_eq!(
            cfg.remove_by_label("GitHub"),
            Err(ConfigError::EntryNotFound)
        );
    }

    #[test]
    fn config_remove_preserves_insertion_order() {
        let mut cfg = CyberKeyConfig::new();
        cfg.add_entry(TotpEntry::new(0, "A", SECRET).unwrap())
            .unwrap();
        cfg.add_entry(TotpEntry::new(1, "B", SECRET).unwrap())
            .unwrap();
        cfg.add_entry(TotpEntry::new(2, "C", SECRET).unwrap())
            .unwrap();
        cfg.remove_by_label("B").unwrap();
        let labels: heapless::Vec<&str, 10> = cfg.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels.as_slice(), &["A", "C"]);
    }

    #[test]
    fn config_remove_frees_slot_for_reuse() {
        let mut cfg = CyberKeyConfig::new();
        cfg.add_entry(TotpEntry::new(0, "GitHub", SECRET).unwrap())
            .unwrap();
        cfg.remove_by_label("GitHub").unwrap();
        // finger_id 0 must be available again after removal.
        assert!(
            cfg.add_entry(TotpEntry::new(0, "GitLab", SECRET).unwrap())
                .is_ok()
        );
    }

    // ── CyberKeyConfig::find_by_finger_id ────────────────────────────────────

    #[test]
    fn config_find_hit_and_miss() {
        let mut cfg = CyberKeyConfig::new();
        cfg.add_entry(TotpEntry::new(3, "Vault", SECRET).unwrap())
            .unwrap();
        assert!(cfg.find_by_finger_id(3).is_some());
        assert!(cfg.find_by_finger_id(7).is_none());
    }

    // ── CyberKeyConfig::iter ──────────────────────────────────────────────────

    #[test]
    fn config_iter_insertion_order() {
        let mut cfg = CyberKeyConfig::new();
        cfg.add_entry(TotpEntry::new(0, "first", SECRET).unwrap())
            .unwrap();
        cfg.add_entry(TotpEntry::new(1, "second", SECRET).unwrap())
            .unwrap();
        cfg.add_entry(TotpEntry::new(2, "third", SECRET).unwrap())
            .unwrap();
        let labels: heapless::Vec<&str, 10> = cfg.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels.as_slice(), &["first", "second", "third"]);
    }
}
