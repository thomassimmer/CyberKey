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
← {"ok":true,"entries":[{"slot":0,"label":"GitHub","secret":"JBSWY3D..."},{"slot":1,"label":"AWS","secret":"AAAA..."}]}
```

Secrets are truncated/masked in the response for display. The firmware never sends the full secret over the wire.

### `add_entry`

```json
→ {"cmd":"add_entry","label":"GitHub","secret":"JBSWY3DPEHPK3PXP"}
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
← {"ok":false,"error":"enrollment failed: bad image on step 2"}
```

**Atomicity guarantee**: if enrollment succeeds on the sensor but the NVS write fails, the firmware deletes the template before returning the error. The caller never sees a state where the sensor has a template with no matching NVS entry.

### `remove_entry`

```json
→ {"cmd":"remove_entry","slot":0}
← {"ok":true}
```

Deletes the NVS entry for slot N and then removes the fingerprint template from the sensor. If the device loses power between these two steps, the NVS entry is gone but the sensor template persists as an orphan. The device treats the slot as empty (the sensor template is unreachable). Recovery: factory reset.

### `factory_reset`

```json
→ {"cmd":"factory_reset","confirm":"RESET"}
← {"ok":true}
```

The `confirm` field must be exactly `"RESET"` (case-sensitive). Erases all NVS entries and all fingerprint templates. The device reboots in an unconfigured state.

### `allow_pairing`

```json
→ {"cmd":"allow_pairing"}
← {"ok":true}
```

Opens the BLE pairing window for 60 seconds. The new passkey is displayed on the LCD. Equivalent to a Button B long-press.

---

## Session Authentication

The CLI interface is gated behind fingerprint authentication:

1. On first command (other than `ping` and `sync_clock`), the firmware prompts for `unlock`.
2. `unlock` triggers fingerprint identification. The firmware waits up to 30 seconds for a match.
3. On match, a 5-minute session is started. All commands within the session are accepted.
4. On session timeout, the next command requires re-authentication.

This prevents a USB-connected malicious host from exfiltrating TOTP secrets without physical access to the enrolled fingers.

**Note**: `sync_clock` and `ping` bypass authentication. Clock sync is needed before TOTP is valid, and a time-of-check/time-of-use gap between authentication and clock sync would be confusing to handle.

---

## Enrollment Implementation Notes

The `add_entry` flow uses two FreeRTOS tasks:

- **CLI task** (runs on a FreeRTOS thread): receives the command, sends it to the main loop via `mpsc::sync_channel`, then blocks waiting for enrollment events.
- **Main loop**: drives the fingerprint sensor (calls `fp.poll_enroll_ack()` on each tick), sends progress back to the CLI task via the channel.

The channel is an `mpsc::sync_channel` — bounded, blocking send (capacity 1). The main loop blocks if the CLI task is not consuming events fast enough, which provides natural backpressure.

This is why enrollment cannot be triggered from the main loop and the CLI task simultaneously: the main loop owns the fingerprint driver, and only one enrollment can be in progress at a time. The CLI task's `add_entry` command sets a flag that the main loop checks before starting any other fingerprint operation.
