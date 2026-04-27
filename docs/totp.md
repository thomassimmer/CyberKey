# TOTP Generation

## How TOTP Works (RFC 6238)

TOTP (Time-based One-Time Password) is defined in RFC 6238. Three steps:

**1. Time counter** — divide the current Unix timestamp by 30 (round down). This gives a counter that increments every 30 seconds. Both the device and the verification server compute the same counter independently, as long as their clocks agree.

**2. HMAC-SHA1** — feed the secret and the counter into HMAC-SHA1. The output is always 20 bytes. Same inputs → same output.

**3. Dynamic truncation** — extract 6 digits from those 20 bytes:
- Read the last byte of the HMAC and keep its 4 low bits → this is an offset (0–15).
- Take 4 bytes starting at that offset.
- Clear the sign bit (`& 0x7FFFFFFF`), then take the result modulo 1 000 000.

The offset comes from the HMAC itself so it varies per code, which avoids always slicing the same 4 bytes and introducing a statistical bias.

The result is a 6-digit number valid for 30 seconds.

**The secret is a base32-encoded byte string** (e.g., `JBSWY3DPEHPK3PXP`). This is what authenticator apps display as a QR code — the underlying data is just base32.

---

## Implementation in `cyberkey-core`

The TOTP logic lives in `crates/cyberkey-core/src/totp.rs`. Dependencies:

- `hmac` (RustCrypto) — HMAC generic over any hash
- `sha1` (RustCrypto) — SHA-1 implementation
- `heapless` — stack-allocated buffers (no `alloc`)

The crate is declared `#![no_std]` for firmware compatibility, with a conditional carve-out:

```rust
#![cfg_attr(not(test), no_std)]
```

This means `std` is available during `cargo test` (on your laptop), but not when compiled for the ESP32. This is the standard pattern for embedded crates that need to be unit-tested on a host machine.

### Why Not Use `totp-rs`?

`totp-rs` v5+ uses `Vec<u8>` and `String` internally. These require a heap allocator. The ESP32 firmware is `no_std` with no allocator, so `totp-rs` cannot be used in firmware code.

TOTP is ~40 lines of math — simple enough to implement directly on top of RustCrypto. Rolling our own gives us full control over error types and keeps the dependency tree small.

---

## Why `heapless` Instead of `alloc`

`heapless` provides data structures with **compile-time fixed capacity** that live on the stack:

```rust
use heapless::Vec;

// Exactly 10 TOTP entries, no heap allocation
let entries: Vec<TotpEntry, 10> = Vec::new();
```

**Why no heap allocator?**

In embedded systems, dynamic memory allocation introduces fragmentation. Over days or weeks of runtime, an allocator running on constrained RAM can reach a state where there is enough free memory in total but no single contiguous block large enough for a new allocation. The result is an unpredictable crash.

With `heapless`, the memory footprint is fixed and known at compile time. The firmware cannot run out of memory for TOTP entries — it can store at most 10, and that is enforced statically by the type system.

The cap of 10 also has a UX rationale: one per finger on both hands, roughly. Using more than 10 requires thinking about which services matter most, which is a healthy constraint.

---

## Clock Sync

TOTP requires both the device and the verification server to agree on the current time within ±30 seconds. The ESP32's internal RTC drifts and loses its state when unpowered.

The CLI syncs the clock at the start of every session:

```
CLI → firmware: {"cmd":"sync_clock","timestamp":1700000000,"tz_offset_secs":7200}
firmware:
  1. writes timestamp to BM8563 RTC via I2C
  2. calls settimeofday to update system clock
  3. stores tz_offset in NVS
firmware → CLI: {"ok":true}
```

After this, the BM8563 keeps ticking even when the ESP32 is off. On next boot, the firmware reads the RTC and calls `settimeofday` before TOTP generation is enabled.

Without a sync, the device falls back to the `BUILD_TIME` constant embedded at compile time. This means codes generated more than a few minutes after flashing are likely wrong until the CLI syncs.

---

## HID Keycode Mapping

### The Problem with Number-Row Keycodes

USB HID keycodes are **physical key positions**, not characters. The USB HID page 0x07 assigns usage `0x1E` to the `1` key — but on an AZERTY keyboard, that key types `à` (ampersand in shifted mode), not `1`.

If CyberKey typed a TOTP code using number-row keycodes (0x1E–0x27), the code would come out wrong on any non-QWERTY keyboard.

### The Fix: Numpad Keycodes

USB HID also has numpad usages (0x59–0x62 for digits 0–9 on the numeric keypad). These are always interpreted as digits by the OS, regardless of the active keyboard layout, because the numpad has no layout-dependent interpretation.

```rust
// In cyberkey-hid: digits typed via numpad
fn digit_to_numpad_keycode(d: u8) -> u8 {
    match d {
        0 => 0x62, // KP_0
        1 => 0x59, // KP_1
        // ...
        9 => 0x61, // KP_9
        _ => 0x00,
    }
}
```

TOTP codes (6 digits) are always sent via numpad keycodes. Other text (labels, prompts) uses the standard ASCII map in `cyberkey-hid` with the assumption that the host uses a QWERTY-compatible layout.

### ASCII Map Structure

`cyberkey-hid/src/lib.rs` contains a 127-entry lookup table indexed by ASCII byte value. Each entry is `(modifier: u8, keycode: u8)`. For uppercase letters, `modifier = 0x02` (Left Shift). For unmapped bytes (control characters), both fields are `0x00`.

The table is `const` — it is computed at compile time and lives in read-only flash. No runtime initialization.
