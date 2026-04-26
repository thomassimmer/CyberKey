//! `protocol` — wire protocol for the CyberKey USB serial link.
//!
//! All messages are newline-terminated JSON objects (`\n` acts as the frame
//! delimiter on the serial line).
//!
//! ## CLI → Firmware (commands)
//!
//! Commands are discriminated by a `"cmd"` field:
//!
//! ```text
//! {"cmd":"list_entries"}
//! {"cmd":"add_entry","label":"GitHub","secret_b32":"JBSWY3DPEHPK3PXP"}
//! {"cmd":"remove_entry","label":"GitHub"}
//! {"cmd":"generate_totp","slot":0}
//! {"cmd":"sync_clock","timestamp":1700000000}
//! {"cmd":"factory_reset","confirm":"RESET"}
//! {"cmd":"allow_pairing"}
//! ```
//!
//! ## Firmware → CLI (responses & events)
//!
//! Responses either carry `"ok": true/false` or an `"event"` field:
//!
//! ```text
//! {"ok":true,"entries":[{"slot":0,"label":"GitHub","secret_masked":"JBSW********************"}]}
//! {"ok":true,"slot":0}
//! {"ok":true,"code":123456}
//! {"event":"enroll_step","step":1,"total":3,"state":"place_finger"}
//! {"event":"enroll_step","step":1,"total":3,"state":"lift_finger"}
//! {"ok":true}
//! {"ok":false,"error":"entry_not_found"}
//! {"version":"v0.1.0"}
//! ```
//!
//! ## Testability
//!
//! [`encode_command`] and [`decode_response`] operate on plain byte slices and
//! are fully testable without a connected device.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ── Commands ──────────────────────────────────────────────────────────────────

/// A command sent from the CLI to the firmware over USB serial.
///
/// Serialised via `serde` with an internal `"cmd"` tag and `snake_case`
/// field/variant names so that the JSON output matches the wire spec exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Request the full list of configured entries.
    ListEntries,

    /// Add a new entry and enroll the associated fingerprint.
    ///
    /// The slot index is **auto-assigned** by the firmware; the CLI must not
    /// send a `finger_id` field. Enrollment events stream back before the
    /// final `{"ok":true,"slot":N}` response.
    AddEntry { label: String, secret_b32: String },

    /// Remove an existing entry by its service label (case-sensitive).
    RemoveEntry { label: String },

    /// Ask the firmware to compute a TOTP code for the given slot using the
    /// device's current RTC time. Sync the clock first for accurate results.
    GenerateTotp { slot: u8 },

    /// Synchronise the device RTC with the host Unix timestamp (seconds since
    /// the UNIX epoch) and the local UTC offset so the display shows local
    /// time. Should be sent once at the start of every config session.
    SyncClock { timestamp: u64, tz_offset_secs: i32 },

    /// Permanently erase all fingerprints and TOTP secrets, then reboot.
    ///
    /// The `confirm` field **must** equal the string `"RESET"` exactly;
    /// the firmware rejects any other value. The CLI layer is responsible for
    /// obtaining explicit confirmation from the user before sending this.
    FactoryReset { confirm: String },

    /// Open the BLE pairing window for one pairing attempt.
    AllowPairing,
}

// ── Response types ────────────────────────────────────────────────────────────

/// Summary information for a single configured TOTP entry, as returned by the
/// firmware in a `list_entries` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryInfo {
    /// Internal slot index (0–9), auto-assigned by the firmware.
    pub slot: u8,
    /// Human-readable service label (max 32 chars).
    pub label: String,
    /// TOTP secret — first 4 chars visible, remainder replaced with `*`.
    /// The firmware never transmits the full plaintext secret over serial.
    pub secret_masked: String,
}

/// The user-action required during a fingerprint capture step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollState {
    /// The user should press their finger onto the sensor.
    PlaceFinger,
    /// The user should lift their finger off the sensor.
    LiftFinger,
}

// ── Raw wire deserialization ──────────────────────────────────────────────────

/// Flat representation of any JSON object the firmware might send.
///
/// All fields are `Option` so that a single `serde_json::from_slice` call can
/// parse any wire message without prior knowledge of its shape. Discrimination
/// into a [`DeviceMessage`] variant happens in [`DeviceMessage::try_from`].
#[derive(Debug, Deserialize)]
struct RawMessage {
    // ── Enrollment event fields ──────────────────────────────────────────────
    event: Option<String>,
    step: Option<u8>,
    total: Option<u8>,
    state: Option<EnrollState>,

    // ── Common response field ────────────────────────────────────────────────
    ok: Option<bool>,

    // ── Variant-specific payload fields ─────────────────────────────────────
    /// Populated by `list_entries` responses.
    entries: Option<Vec<EntryInfo>>,
    /// Populated by `add_entry` responses.
    slot: Option<u8>,
    /// Populated by `generate_totp` responses.
    code: Option<u32>,
    /// Populated by error responses.
    error: Option<String>,

    // ── Firmware greeting ────────────────────────────────────────────────────
    version: Option<String>,
}

// ── High-level device message ─────────────────────────────────────────────────

/// A fully-decoded message received from the firmware.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceMessage {
    /// A streaming fingerprint capture event during `add_entry` enrollment.
    EnrollStep {
        step: u8,
        total: u8,
        state: EnrollState,
    },

    /// Successful `list_entries` response.
    EntryList { entries: Vec<EntryInfo> },

    /// Successful `add_entry` response — `slot` is the auto-assigned index.
    AddEntryOk { slot: u8 },

    /// Successful `generate_totp` response — `code` is a 6-digit TOTP value.
    TotpCode { code: u32 },

    /// Generic success for commands that return no additional payload
    /// (`remove_entry`, `sync_clock`, `factory_reset`, `allow_pairing`).
    Ok,

    /// Error response from the firmware.
    Error { error: String },

    /// Firmware greeting sent automatically on serial connect.
    Greeting { version: String },
}

impl TryFrom<RawMessage> for DeviceMessage {
    type Error = anyhow::Error;

    fn try_from(raw: RawMessage) -> Result<Self> {
        // ── Enrollment event ─────────────────────────────────────────────────
        // Discriminated by the presence of the `event` field — no `ok` needed.
        if let Some(event_name) = raw.event {
            if event_name == "enroll_step" {
                return Ok(DeviceMessage::EnrollStep {
                    step: raw.step.context("enroll_step missing `step`")?,
                    total: raw.total.context("enroll_step missing `total`")?,
                    state: raw.state.context("enroll_step missing `state`")?,
                });
            }
            anyhow::bail!("unknown firmware event type: {event_name:?}");
        }

        // ── Firmware greeting ─────────────────────────────────────────────────
        // Has `version` but no `ok`.
        if let Some(version) = raw.version {
            return Ok(DeviceMessage::Greeting { version });
        }

        // ── All remaining variants require `ok` ───────────────────────────────
        let ok = raw.ok.context("`ok` field absent in device response")?;

        if let Some(entries) = raw.entries {
            return Ok(DeviceMessage::EntryList { entries });
        }
        if let Some(slot) = raw.slot {
            return Ok(DeviceMessage::AddEntryOk { slot });
        }
        if let Some(code) = raw.code {
            return Ok(DeviceMessage::TotpCode { code });
        }
        if let Some(error) = raw.error {
            return Ok(DeviceMessage::Error { error });
        }

        if ok {
            Ok(DeviceMessage::Ok)
        } else {
            anyhow::bail!("device returned `ok: false` without an error message")
        }
    }
}

// ── Codec ─────────────────────────────────────────────────────────────────────

/// Encodes a [`Command`] as a newline-terminated UTF-8 JSON byte sequence.
///
/// The trailing `\n` acts as the message delimiter on the serial line.
///
/// # Errors
///
/// Fails only if `serde_json` serialisation fails — which cannot happen for
/// the well-defined `Command` type in practice.
pub fn encode_command(cmd: &Command) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(cmd).context("Command serialisation failed")?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decodes a firmware response from a UTF-8 JSON byte slice.
///
/// A trailing newline, if present, is silently ignored by `serde_json`.
///
/// # Errors
///
/// - JSON parse error (malformed or non-UTF-8 payload).
/// - Missing required fields for the detected message type.
/// - Unknown `event` type.
pub fn decode_response(bytes: &[u8]) -> Result<DeviceMessage> {
    let raw: RawMessage =
        serde_json::from_slice(bytes).context("Failed to parse device message as JSON")?;
    DeviceMessage::try_from(raw)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── encode_command ────────────────────────────────────────────────────────

    /// Every encoded command must end with exactly one newline.
    #[test]
    fn encode_ends_with_newline() {
        let cmd = Command::ListEntries;
        let bytes = encode_command(&cmd).unwrap();
        assert_eq!(*bytes.last().unwrap(), b'\n');
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
    }

    /// `list_entries` encodes to `{"cmd":"list_entries"}`.
    #[test]
    fn encode_list_entries() {
        let bytes = encode_command(&Command::ListEntries).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(json["cmd"], "list_entries");
    }

    /// `add_entry` encodes label and secret — no `finger_id` field.
    #[test]
    fn encode_add_entry_fields() {
        let cmd = Command::AddEntry {
            label: "GitHub".to_string(),
            secret_b32: "JBSWY3DPEHPK3PXP".to_string(),
        };
        let bytes = encode_command(&cmd).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();

        assert_eq!(json["cmd"], "add_entry");
        assert_eq!(json["label"], "GitHub");
        assert_eq!(json["secret_b32"], "JBSWY3DPEHPK3PXP");
        assert!(
            json.get("finger_id").is_none(),
            "finger_id must NOT be present"
        );
        assert!(json.get("slot").is_none(), "slot must NOT be present");
    }

    /// `remove_entry` encodes the label.
    #[test]
    fn encode_remove_entry() {
        let cmd = Command::RemoveEntry {
            label: "AWS".to_string(),
        };
        let bytes = encode_command(&cmd).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(json["cmd"], "remove_entry");
        assert_eq!(json["label"], "AWS");
    }

    /// `factory_reset` must include `"confirm":"RESET"` on the wire.
    #[test]
    fn encode_factory_reset_confirm_field() {
        let cmd = Command::FactoryReset {
            confirm: "RESET".to_string(),
        };
        let bytes = encode_command(&cmd).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(json["cmd"], "factory_reset");
        assert_eq!(json["confirm"], "RESET");
    }

    /// `sync_clock` timestamp round-trip at boundary value 0.
    #[test]
    fn encode_sync_clock_timestamp_zero() {
        let cmd = Command::SyncClock { timestamp: 0 };
        let bytes = encode_command(&cmd).unwrap();
        let decoded: Command = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(decoded, cmd);
    }

    /// `sync_clock` timestamp round-trip at u64::MAX.
    #[test]
    fn encode_sync_clock_timestamp_max() {
        let cmd = Command::SyncClock {
            timestamp: u64::MAX,
        };
        let bytes = encode_command(&cmd).unwrap();
        let decoded: Command = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(decoded, cmd);
    }

    /// `generate_totp` encodes the slot number.
    #[test]
    fn encode_generate_totp_slot() {
        let cmd = Command::GenerateTotp { slot: 3 };
        let bytes = encode_command(&cmd).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(json["cmd"], "generate_totp");
        assert_eq!(json["slot"], 3);
    }

    // ── decode_response ───────────────────────────────────────────────────────

    /// `list_entries` response with one entry.
    #[test]
    fn decode_entry_list_single() {
        let json = br#"{"ok":true,"entries":[{"slot":0,"label":"GitHub","secret_masked":"JBSW********************"}]}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(
            msg,
            DeviceMessage::EntryList {
                entries: vec![EntryInfo {
                    slot: 0,
                    label: "GitHub".to_string(),
                    secret_masked: "JBSW********************".to_string(),
                }],
            }
        );
    }

    /// `list_entries` response with an empty list.
    #[test]
    fn decode_entry_list_empty() {
        let json = br#"{"ok":true,"entries":[]}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(msg, DeviceMessage::EntryList { entries: vec![] });
    }

    /// `add_entry` success response includes the assigned slot.
    #[test]
    fn decode_add_entry_ok() {
        let json = br#"{"ok":true,"slot":0}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(msg, DeviceMessage::AddEntryOk { slot: 0 });
    }

    /// `generate_totp` response carries the 6-digit code.
    #[test]
    fn decode_totp_code() {
        let json = br#"{"ok":true,"code":996554}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(msg, DeviceMessage::TotpCode { code: 996554 });
    }

    /// Generic success response maps to `DeviceMessage::Ok`.
    #[test]
    fn decode_generic_ok() {
        let json = br#"{"ok":true}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(msg, DeviceMessage::Ok);
    }

    /// Error response with an error string.
    #[test]
    fn decode_error_response() {
        let json = br#"{"ok":false,"error":"entry_not_found"}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(
            msg,
            DeviceMessage::Error {
                error: "entry_not_found".to_string(),
            }
        );
    }

    /// Enrollment step — `place_finger` state.
    #[test]
    fn decode_enroll_step_place_finger() {
        let json = br#"{"event":"enroll_step","step":1,"total":3,"state":"place_finger"}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(
            msg,
            DeviceMessage::EnrollStep {
                step: 1,
                total: 3,
                state: EnrollState::PlaceFinger,
            }
        );
    }

    /// Enrollment step — `lift_finger` state.
    #[test]
    fn decode_enroll_step_lift_finger() {
        let json = br#"{"event":"enroll_step","step":2,"total":3,"state":"lift_finger"}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(
            msg,
            DeviceMessage::EnrollStep {
                step: 2,
                total: 3,
                state: EnrollState::LiftFinger,
            }
        );
    }

    /// Firmware greeting carries the version string.
    #[test]
    fn decode_firmware_greeting() {
        let json = br#"{"version":"v0.1.0"}"#;
        let msg = decode_response(json).unwrap();
        assert_eq!(
            msg,
            DeviceMessage::Greeting {
                version: "v0.1.0".to_string(),
            }
        );
    }

    /// A trailing newline in the input is tolerated (mirrors what `read_line` returns).
    #[test]
    fn decode_tolerates_trailing_newline() {
        let json = b"{\"ok\":true}\n";
        let msg = decode_response(json).unwrap();
        assert_eq!(msg, DeviceMessage::Ok);
    }

    /// Malformed JSON must return an error.
    #[test]
    fn decode_malformed_json_returns_error() {
        let bad = b"not json at all";
        assert!(decode_response(bad).is_err());
    }

    /// An unknown `event` type must return an error.
    #[test]
    fn decode_unknown_event_returns_error() {
        let json = br#"{"event":"unknown_event_type"}"#;
        assert!(decode_response(json).is_err());
    }

    /// A message with `ok:false` and no `error` field must return an error.
    #[test]
    fn decode_ok_false_no_error_returns_error() {
        let json = br#"{"ok":false}"#;
        assert!(decode_response(json).is_err());
    }

    /// `list_entries` with multiple entries preserves insertion order.
    #[test]
    fn decode_entry_list_preserves_order() {
        let json = br#"{
            "ok": true,
            "entries": [
                {"slot":0,"label":"GitHub","secret_masked":"JBSW********************"},
                {"slot":1,"label":"AWS","secret_masked":"OJA3********************"},
                {"slot":2,"label":"VPN corp","secret_masked":"K5QW********************"}
            ]
        }"#;
        let msg = decode_response(json).unwrap();
        if let DeviceMessage::EntryList { entries } = msg {
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].label, "GitHub");
            assert_eq!(entries[1].label, "AWS");
            assert_eq!(entries[2].label, "VPN corp");
        } else {
            panic!("expected EntryList, got {msg:?}");
        }
    }

    // ── Command round-trips ───────────────────────────────────────────────────

    /// Every command variant should survive an encode → JSON decode round-trip.
    #[test]
    fn command_round_trip_all_variants() {
        let commands = vec![
            Command::ListEntries,
            Command::AddEntry {
                label: "Svc".to_string(),
                secret_b32: "AAAA".to_string(),
            },
            Command::RemoveEntry {
                label: "Svc".to_string(),
            },
            Command::GenerateTotp { slot: 7 },
            Command::SyncClock {
                timestamp: 1_700_000_000,
            },
            Command::FactoryReset {
                confirm: "RESET".to_string(),
            },
            Command::AllowPairing,
        ];

        for cmd in commands {
            let bytes = encode_command(&cmd).unwrap();
            // strip trailing newline before deserialising
            let decoded: Command = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
            assert_eq!(decoded, cmd, "round-trip failed for {cmd:?}");
        }
    }
}
