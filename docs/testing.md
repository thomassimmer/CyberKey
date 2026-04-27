# Testing

## Strategy

CyberKey's crate split is designed so that all business logic is testable on a laptop without hardware.

The rule: **if code does not touch a hardware peripheral, it belongs in a `no_std` portable crate with unit tests**. Firmware (`firmware/`) is a thin integration layer that wires portable crates to hardware drivers.

---

## What Can Be Tested on a Laptop

### `cyberkey-core` — TOTP + Config

```bash
cargo test --package cyberkey-core
```

- TOTP known-vector tests (RFC 6238 Appendix B — SHA-1 at T=0, T=1, etc.)
- Config validation (duplicate `finger_id`, label length, secret length)
- Config serialization/deserialization roundtrip
- BCD encoding/decoding for RTC timestamps

The crate uses `#![cfg_attr(not(test), no_std)]`. Tests compile with `std` enabled, firmware compilation uses `no_std`. Same code, different feature sets.

### `fingerprint2-rs` — Sensor Driver

```bash
cargo test --package fingerprint2-rs
```

~28 unit tests covering:

- Packet framing (serialize/deserialize)
- Checksum computation for known packet sequences
- Command encoding (enroll, identify, LED color)
- Error parsing (sensor error codes → `FingerprintError` variants)
- Wakeup packet detection (`is_wakeup_packet` for the exact 12-byte sequence)
- Mock UART round-trips (send command, fake sensor response, check parsed result)

The driver is generic over `embedded_hal_nb::serial::{Read, Write}`. Tests use a `MockUart` that is two `Vec<u8>` buffers (RX and TX). No hardware needed.

### `cyberkey-hid` — Keycode Table

```bash
cargo test --package cyberkey-hid
```

- All printable ASCII bytes (0x20–0x7E) map to a non-zero keycode
- Uppercase letters have the Shift modifier set
- Digits and symbols match the USB HID usage table
- Numpad keycodes cover 0–9 without gaps

The table is `const`, so tests catch misaligned entries at compile time.

### `cyberkey-cli` — CLI + Protocol

```bash
cargo test --package cyberkey-cli
```

Light coverage:

- JSON command serialization matches expected wire format
- JSON response deserialization handles `ok:false` with error field
- Secret masking (the first N characters of a secret are hidden in `list_entries` output)

End-to-end CLI tests (against a real device) are manual — see below.

---

## Running All Portable Tests

```bash
# From workspace root
cargo test --exclude firmware
```

This runs all tests for all crates except the firmware crate, which requires the Xtensa toolchain and cannot be cross-compiled to your host target.

---

## Firmware (Hardware Required)

The firmware crate has no unit tests. Testing is manual smoke testing on a physical device.

### Environment Setup

```bash
# One-time: install the Xtensa toolchain
cargo install espup
espup install
source ~/.espup/export-esp.sh
```

```bash
cd firmware
cargo run --release   # builds, flashes, and opens serial monitor
```

### Smoke Test Checklist

**Boot:**
- [ ] Device powers on, status bar shows time + battery
- [ ] If no bonds in NVS: LCD shows pairing screen with passkey
- [ ] If bonds exist: LCD shows "Connecting..." then "Connected" (or "Waiting..." if host is off)

**Buttons:**
- [ ] Button B short press: triggers "Hello!" HID test (types "Hello!" if BLE connected)
- [ ] Button B long-press (3 s): opens BLE pairing window for 60 s
- [ ] Button A long-press (3 s): prompts to confirm bond clear
- [ ] Button A long-press × 2 within 10 s: erases bonds, reboots

**BLE Pairing:**
- [ ] Host OS detects CyberKey in Bluetooth settings
- [ ] Host prompts for passkey; enter the code shown on LCD
- [ ] Bond completes; LCD shows "Connected"
- [ ] Reboot device; host reconnects automatically (no re-pairing)

**Fingerprint + TOTP:**
- [ ] CLI: add an entry (`add_entry`, label = "Test", secret = valid base32)
- [ ] Complete 3-step enrollment on device
- [ ] Place enrolled finger on sensor → TOTP code appears in focused text field
- [ ] Place wrong finger → red LED flash, no typing

**CLI:**
- [ ] `cyberkey-cli` auto-detects port, sends `sync_clock`, shows menu
- [ ] `list_entries` shows enrolled entries with masked secrets
- [ ] `remove_entry` removes an entry; subsequent finger match returns no-match
- [ ] `factory_reset` (type "RESET" to confirm) wipes all entries and reboots

**Clock sync:**
- [ ] After `sync_clock`, LCD shows correct local time
- [ ] After reboot without CLI session, LCD still shows correct local time (RTC persisted)
- [ ] TOTP codes match a reference authenticator app

---

## Test Coverage Gaps

| Area | Gap | Notes |
|------|-----|-------|
| CLI end-to-end | No automated serial loopback tests | Would require a hardware-in-the-loop setup |
| BLE pairing | Manual only | No emulated BLE host |
| NVS encryption | Not directly tested | Tested implicitly by add/list/remove on a flashed device |
| Power management | Manual only | Light sleep behavior requires oscilloscope to verify current draw |
| Long-press timing | Not unit-tested | Threshold (3 s = 150 polls × 20 ms) is a constant; test would be a tautology |
