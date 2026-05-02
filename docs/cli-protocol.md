# CLI Protocol

## Overview

The CLI (`cyberkey-cli`) communicates with the firmware over USB-C serial (115.2k bps, 8N1). The protocol is **newline-delimited JSON** — each message is a single JSON object followed by `\n`.

The CLI binary is a standard `std` Rust program that runs on macOS, Linux, or Windows. It handles enrollment, clock sync, and bond management through this protocol. The BLE pairing itself happens over BLE and is initiated by the host OS.

---

## Why JSON, Not Binary

A compact binary protocol (e.g., 1-byte opcode + length-prefixed fields) would be smaller and faster. JSON was chosen for v0.1 because:

1. **Debuggable without tooling**: open any serial terminal and read human-readable commands. `cat /dev/tty.usbserial-3` works.
2. **No custom parser**: `serde_json` handles everything. A binary protocol requires custom encoding on both ends and careful handling of byte order, padding, and versioning.
3. **Latency is irrelevant here**: 115.2k bps is fast enough for configuration commands. The largest message (enrollment step) is under 100 bytes.
4. **Easy to extend**: adding a new field to a JSON object is backwards-compatible. Adding a field to a fixed binary struct is not.

The protocol can be replaced with a compact binary format later without changing the crate boundaries.

---

## Message Format

### Command (CLI → firmware)

```json
{"cmd": "command_name", "field1": "value1", ...}
```

The `cmd` field is the discriminant. All other fields are command-specific.

### Response (firmware → CLI)

```json
{"ok": true}
{"ok": false, "error": "human-readable description"}
```

Successful responses may include additional fields (e.g., `"entries"`, `"slot"`).

---

## Command Reference

### `ping`

```json
→ {"cmd":"ping"}
← {"ok":true}
```

Used to verify the connection and identify the firmware. The firmware sends a greeting string on connect (version + board info) before any command.

### `sync_clock`

```json
→ {"cmd":"sync_clock","timestamp":1700000000,"tz_offset_secs":7200}
← {"ok":true}
```

Sets the BM8563 RTC to the given Unix timestamp and stores the UTC offset in NVS. The CLI always sends this first.

`timestamp`: Unix epoch seconds (UTC).
`tz_offset_secs`: local UTC offset in seconds (e.g., 3600 = UTC+1, -18000 = UTC-5). Used only for the display — TOTP generation always uses UTC.

### `list_entries`

```json
→ {"cmd":"list_entries"}
← {"ok":true,"entries":[{"slot":0,"label":"GitHub","secret_masked":"JBSW********************"},{"slot":1,"label":"AWS","secret_masked":"AAAA********************"}]}
```

Secrets are fully masked in the response — the `secret_masked` field contains only `*` characters. The firmware never sends the plaintext secret over the wire.

### `add_entry`

```json
→ {"cmd":"add_entry","label":"GitHub","secret_b32":"JBSWY3DPEHPK3PXP"}
```

This command initiates fingerprint enrollment. The firmware streams enrollment progress events before the final response:

```json
← {"event":"enroll_step","step":1,"total":3,"state":"place_finger"}
   (user places finger)
← {"event":"enroll_step","step":1,"total":3,"state":"lift_finger"}
   (user lifts finger)
← {"event":"enroll_step","step":2,"total":3,"state":"place_finger"}
   (user places finger again)
← {"event":"enroll_step","step":2,"total":3,"state":"lift_finger"}
← {"event":"enroll_step","step":3,"total":3,"state":"place_finger"}
← {"event":"enroll_step","step":3,"total":3,"state":"lift_finger"}
← {"ok":true,"slot":0}
```

On failure (e.g., bad fingerprint capture, NVS full):

```json
← {"ok":false,"error":"enrollment failed"}
```

If the finger is already enrolled in another slot:

```json
← {"ok":false,"error":"duplicate_finger"}
```

The CLI detects `"duplicate_finger"` by string match on the error field and shows a dedicated message without offering a retry (re-trying with the same finger would produce the same result).

**Atomicity guarantee**: if enrollment fails for any reason — including a duplicate detection — the firmware removes the NVS entries it had already written (`slot_N` / `label_N`) before returning the error. The caller never sees a state where the sensor has a template with no matching NVS entry, nor an NVS entry with no matching sensor template.

### `remove_entry`

```json
→ {"cmd":"remove_entry","label":"GitHub"}
← {"ok":true}
```

Removes an entry by its service label (case-sensitive). The firmware looks up the slot for that label, deletes the NVS entry first (`slot_N` + `label_N`), then removes the fingerprint template from the sensor. If the device loses power between these two steps, the NVS entry is gone but the sensor template persists as an orphan. The device treats the slot as empty (the sensor template is unreachable). Recovery: factory reset.

### `factory_reset`

```json
→ {"cmd":"factory_reset","confirm":"RESET"}
← {"ok":true}
```

The `confirm` field must be exactly `"RESET"` (case-sensitive).

The firmware responds `{"ok":true}` immediately, then the main loop picks up the request and performs the full wipe before rebooting. Data cleared:

- All TOTP slots (`slot_0`–`slot_9`) and their labels (`label_0`–`label_9`) from NVS
- Timezone offset (`tz_offset`) from NVS
- All fingerprint templates from the sensor's internal flash
- All BLE bond data (LTK, IRK, CCCD) from NimBLE's NVS namespace

The device reboots in an unconfigured state. The BLE pairing window opens automatically on first boot after reset.

**What is not cleared**: the BM8563 RTC hardware timestamp. It persists as long as the device has power. Re-run `sync_clock` after reset if the displayed time matters.

### `allow_pairing`

```json
→ {"cmd":"allow_pairing"}
← {"ok":true}
```

Opens the BLE pairing window for 60 seconds. The new passkey is displayed on the LCD. Equivalent to a Button B short-press.

---

## Session Authentication

The CLI interface is gated behind fingerprint authentication once at least one entry is enrolled. A fresh device with no entries is open (bootstrap mode).

**Authentication flow:**

1. The CLI sends `{"cmd":"unlock"}` explicitly before any protected command.
2. The firmware triggers fingerprint identification on the device (up to 30 seconds).
3. On match, a 5-minute session is started. All subsequent commands are accepted without re-authentication.
4. On session timeout, the next protected command returns `{"ok":false,"error":"cli_locked: send {\"cmd\":\"unlock\"} then place finger"}`.

If a protected command is sent without a prior `unlock`, the firmware returns the same `cli_locked` error immediately without attempting a scan.

This prevents a USB-connected malicious host from exfiltrating TOTP secrets without physical access to the enrolled fingers.

**Note**: `sync_clock` and `ping` bypass authentication. Clock sync is needed before TOTP is valid, and requiring authentication before clock sync would prevent the device from working correctly on first connect.

---

## Enrollment Implementation Notes

### Manual enrollment state machine

Enrollment is driven by a firmware-side state machine using the sensor's low-level commands rather than the high-level `PS_AUTO_ENROLL` opcode. This allows duplicate detection to be integrated into the first capture pass at zero extra cost to the user (no additional finger placement).

For each capture pass the sequence is:

| Step | Command | Purpose |
|------|---------|---------|
| `PS_GET_ENROLL_IMAGE` (0x29) | Capture finger image (enrollment-optimised opcode) |
| `PS_GEN_CHAR` (0x02) | Extract feature set into CharBuffer 1 (odd passes) or CharBuffer 2 (even passes) |
| `PS_SEARCH` (0x04) — **pass 1 only** | Search CharBuffer 1 against the full template library; abort if a different slot matches |

After all passes:

| Step | Command | Purpose |
|------|---------|---------|
| `PS_REG_MODEL` (0x05) | Merge CharBuffer 1 + CharBuffer 2 into a single template (result in CharBuffer 1) |
| `PS_STORE_CHAR` (0x06) | Write CharBuffer 1 to flash at the target slot |

The buffer alternates with each pass (`odd → buf 1, even → buf 2`), so the final `PS_REG_MODEL` always has two complementary captures to merge. Pass 3 overwrites CharBuffer 1 with a fresher, higher-quality capture before the merge.

`PS_STORE_CHAR` uses an extended ACK timeout of 3 000 ms (vs. the standard 500 ms) because writing to the sensor's internal STM32 flash can take over 500 ms.

### ACK event sequence visible to the CLI

For each pass, the firmware emits:

1. **`StartCapture`**: Sensor ready, waiting for the user's finger. Firmware sends `place_finger` event to CLI.
2. **`ImageOk`**: Image captured. On pass 1, this is only emitted after a successful duplicate check. Firmware sends `lift_finger` event immediately (user can lift now).
3. **`LiftOk`**: Finger lift confirmed. Firmware prepares for the next pass.

### FreeRTOS task split

The `add_entry` flow uses two FreeRTOS tasks:

- **CLI task** (runs on a FreeRTOS thread): receives the command, sends it to the main loop via `mpsc::sync_channel`, then blocks waiting for enrollment events.
- **Main loop**: drives the fingerprint sensor (calls `fp.poll_enroll_ack()` on each tick), sends progress back to the CLI task via the channel.

The channel is an `mpsc::sync_channel` — bounded, blocking send (capacity 1). The main loop blocks if the CLI task is not consuming events fast enough, which provides natural backpressure.

This is why enrollment cannot be triggered from the main loop and the CLI task simultaneously: the main loop owns the fingerprint driver, and only one enrollment can be in progress at a time.
