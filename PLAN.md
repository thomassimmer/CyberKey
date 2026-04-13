# CyberKey — Technical Documentation & Development Plan

## Table of Contents

1. [Project Overview](#project-overview)
2. [Hardware](#hardware)
3. [Software Architecture](#software-architecture)
4. [Fingerprint2 UART Protocol](#fingerprint2-uart-protocol)
5. [Roadmap](#roadmap)
6. [Pre-Hardware Preparation Plan](#pre-hardware-preparation-plan)
7. [Risks & Open Questions](#risks--open-questions)

---

## Project Overview

**CyberKey** is a hardware TOTP authenticator secured by fingerprint recognition. The device
behaves as a Bluetooth HID keyboard: after biometric validation, it automatically types the
6-digit OTP code into the target machine.

**Problem statement**: the daily MFA workflow (unlock phone → open app → copy code) creates
unnecessary friction, especially at high repetition rates. The CyberKey reduces this to a
single gesture — placing a finger on the sensor.

**One finger = one service**: each enrolled finger is permanently bound to a single service
at enrollment time. There is no runtime selection step — the finger itself is the selector.
Placing a finger identifies the service, generates its TOTP code, and types it
automatically. The device supports up to 10 entries, which is a deliberate UX constraint
(roughly one per finger) rather than a technical limitation of the sensor hardware (which
supports up to 100 templates). The `finger_id` slot is auto-assigned by the firmware and
is never exposed as a user-facing concept.

---

## Hardware

### Brain — M5StickC PLUS2

| Spec          | Value                                             |
|---------------|---------------------------------------------------|
| SoC           | ESP32-PICO-V3-02                                  |
| Connectivity  | Wi-Fi 802.11 b/g/n + Bluetooth 4.2 (Classic + BLE) |
| Display       | LCD 1.14" 135×240                                 |
| Battery       | Built-in Li-Ion                                   |
| Interface     | USB-C (CP2104/CH9102F USB-to-UART bridge)         |
| Grove port    | PORT.A — pins G32/G33, 5V rail                    |

### Sensor — M5Stack Unit Fingerprint2 (SKU: U203)

| Spec              | Value                                         |
|-------------------|-----------------------------------------------|
| Internal MCU      | STM32G031G8U6                                 |
| Fingerprint module | A-K323CP (capacitive)                        |
| Resolution        | 508 dpi — 80×208 px                           |
| Capacity          | 100 entries                                   |
| Entry size        | 7,262 bytes                                   |
| Communication     | UART 115,200 bps, 8N1                         |
| Connector         | Grove HY2.0-4P (GND / 5V / RX / TX)          |
| LED               | RGB ring, 7 colors                            |
| Power draw        | ~40 mA (active) / ~14 mA (sleep)             |

> **Capacity note**: the sensor supports 100 stored templates. The firmware caps active
> entries at 10 by design (one per finger — see Project Overview). The remaining sensor
> capacity is intentionally unused.

> **Wiring note**: the M5StickC PLUS2 Grove connector supplies 5V on the red wire. TX/RX
> signal lines operate at 3.3V logic, which is compatible with the internal STM32's I/O.

---

## Software Architecture

### Tech Stack

| Layer              | Technology                                          |
|--------------------|-----------------------------------------------------|
| Language           | Rust (edition 2021)                                 |
| Embedded framework | `esp-idf` via `esp-idf-hal` + `esp-idf-svc`        |
| Bluetooth          | `esp32-nimble` (NimBLE Rust bindings)               |
| TOTP crypto        | `totp-rs`                                           |
| Secret storage     | ESP-IDF NVS encrypted partition — AES-256 XTS, key stored in eFuses |
| Time sync          | USB serial — CLI sends Unix timestamp on each config session (`sync_clock` command) |
| Desktop CLI        | `dialoguer` + `tabled` + `serialport`               |

### Repository Structure (Cargo Workspace)

```
cyberkey/
├── Cargo.toml                  ← workspace root
├── PLAN.md
├── crates/
│   ├── cyberkey-core/          ← no_std, fully testable on desktop
│   │   └── (TOTP engine, config schema, shared types)
│   ├── fingerprint2-rs/        ← no_std UART driver (embedded-hal)
│   │   └── (packet codec, command set, generic driver)
│   └── cyberkey-cli/           ← std binary, runs on macOS/Linux
│       └── (dialoguer + tabled + serialport over USB)
├── firmware/                   ← esp-idf crate, wires everything together
│   └── src/main.rs
└── docs/
    └── devlog.md
```

**Separation rationale**:

- `cyberkey-core` and `fingerprint2-rs` are `no_std` → they compile and run under
  `cargo test` on desktop, with no hardware required.
- `cyberkey-cli` is a standard `std` binary, fully independent from the firmware build.
- `firmware` only assembles the above crates with concrete ESP-IDF peripheral drivers.

### Testing Strategy

The workspace is structured so that the vast majority of logic can be tested on a
standard desktop machine (x86_64 / arm64), without any physical hardware.

| Crate              | Test environment              | Primary tool                            |
|--------------------|-------------------------------|-----------------------------------------|
| `cyberkey-core`    | Native desktop (`x86_64`)    | `cargo test`                            |
| `fingerprint2-rs`  | Native desktop + mock UART   | `cargo test` + `MockUart` struct        |
| `cyberkey-cli`     | Native desktop               | `cargo test` (unit + light integration) |
| `firmware`         | ❌ Hardware required          | Manual tests on device                  |

**Core principle**: any logic that can live in `cyberkey-core` or `fingerprint2-rs`
should live there — making it testable without a cable or a chip.

**Mock vs hardware**: a mock is a zero-cost in-memory implementation of a trait (here
`embedded_hal_nb::serial::Read/Write`) that simulates sensor responses via byte buffers.
This is the standard pattern in the embedded Rust ecosystem and requires no hardware.

**CI pipeline**: a GitHub Actions workflow (`.github/workflows/ci.yml`) should be set up
during Step 2, running `cargo test --workspace` and `cargo clippy --workspace` on every
push and pull request. Because `firmware` requires the Xtensa toolchain and cannot be
unit-tested in isolation, the CI matrix targets only `cyberkey-core`, `fingerprint2-rs`,
and `cyberkey-cli`, using the standard `stable` Rust toolchain on `ubuntu-latest`. The
firmware crate should be excluded from the test step via a workspace-level `exclude` or a
dedicated `--package` flag. This ensures fast feedback on all portable logic without
requiring the ESP32 build environment in CI.


### Power Management

**Target architecture (implemented from Phase 3 onward):**

The device spends most of its life waiting for a finger. The following cycle minimises
power draw while remaining instantly responsive:

```
┌─────────────────────────────────────────────────────────────────┐
│ 1. ESP32 enters light sleep                                     │
│ 2. Fingerprint sensor enters sleep (PS_SetWorkMode)             │
│    combined draw: ~0.8 mA (ESP32) + ~14 mA (sensor) ≈ 15 mA   │
│                                                                 │
│ 3. User places finger                                           │
│    → sensor wakes autonomously                                  │
│    → emits 12-byte wakeup packet on UART (Grove)               │
│    → UART interrupt wakes ESP32 from light sleep               │
│                                                                 │
│ 4. ESP32 sends PS_AutoIdentify                                  │
│ 5. Match found → generate TOTP → type via BLE HID (~1–3 s)     │
│    No match → flash red LED, return to sleep                    │
│                                                                 │
│ 6. ESP32 sends PS_SetWorkMode (sensor back to sleep)           │
│ 7. ESP32 returns to light sleep → back to step 1               │
└─────────────────────────────────────────────────────────────────┘
```

**Rough power budget**: the M5StickC PLUS2 battery is ~200 mAh. At ~15 mA average
draw in light sleep, theoretical standby autonomy is ~13 hours. Actual autonomy will
be higher in practice (brief active bursts, not sustained 15 mA). This is acceptable
for a device that is likely docked or carried and used several times a day.

**v0.1 (development phase)**: always-on, no sleep. Power management is introduced
once core features are stable. The always-on note in the CLI section applies only to
this phase.

**Deep sleep (deferred)**: deep sleep would reduce idle draw to ~0.15 mA but requires
GPIO wakeup instead of UART wakeup. G32 and G33 (Grove port) are RTC-capable GPIOs on
the ESP32-PICO-V3-02, so a deep sleep implementation is technically feasible as a future
optimisation — the falling edge of the wakeup packet's start bit would trigger the wake.
Not targeted before v1.0.

**CLI sessions and sleep**: when a CLI session is active (USB connected), light sleep is
suppressed — UART0 must remain fully responsive. Sleep resumes when the session ends.

### CLI ↔ CyberKey Communication

The desktop CLI communicates with the device over **USB serial** (USB-C cable), routed
through the M5StickC PLUS2's onboard USB-to-UART bridge (CP2104/CH9102F). On the host
side, the `serialport` crate handles cross-platform port access
(`/dev/tty.usbserial-*` on macOS, `COM*` on Windows).

#### Session model

There is no explicit "config mode" to activate. The firmware runs a lightweight background
task that listens on UART0 at all times. A CLI session begins when the CLI opens the port
and sends a `ping` command; it ends when the CLI sends `bye` or after a 30-second
inactivity timeout.

**Mutual exclusion — fingerprint vs. CLI**: the two operating modes are treated as
mutually exclusive. While a CLI session is active, fingerprint scanning is suspended. When
the session ends, scanning resumes. This avoids any concurrency or locking complexity at
the cost of a negligible UX limitation (the device cannot authenticate while being
configured — which matches the natural usage pattern anyway).

**Battery behaviour**: when powered from USB, the external supply covers all consumption
and UART0 listening is free. When on battery, the firmware does not initiate CLI sessions
— commands received on UART0 are silently ignored unless a session was already open. In
v0.1, the device runs in always-on mode (no sleep); battery autonomy is a known
limitation to be addressed in a later iteration via light sleep (UART0 can wake the ESP32
from light sleep via interrupt; deep sleep is incompatible and will not be used).

#### Wire protocol — v0.1 (JSON newline-delimited)

Each message is a single JSON object terminated by `\n`. This format is human-readable,
trivially debuggable with any serial terminal, and requires no custom framing logic.

Request/response examples:

```
→ {"cmd":"ping"}
← {"ok":true,"version":"0.1.0"}

→ {"cmd":"list_entries"}
← {"ok":true,"entries":[{"slot":0,"label":"GitHub"},{"slot":1,"label":"AWS"}]}

→ {"cmd":"add_entry","label":"VPN corp","secret_b32":"JBSWY3DPEHPK3PXP"}
← {"ok":true,"slot":2}

→ {"cmd":"remove_entry","label":"VPN corp"}
← {"ok":true}

→ {"cmd":"sync_clock","unix_ts":1718000000}
← {"ok":true}

→ {"cmd":"bye"}
← {"ok":true}
```

Error response shape (any command):

```
← {"ok":false,"error":"entry_not_found"}
```

On the firmware side, `serde-json-core` (a `no_std`-compatible JSON parser) handles
deserialization into fixed-size structs. On the CLI side, standard `serde_json` is used.

> **Future migration**: JSON is verbose for an embedded link. Once the protocol is stable
> and the command set is finalized, migrating to a compact binary format
> (e.g. length-prefixed frames with a 1-byte opcode) should be considered to reduce
> latency and firmware code size. The migration is straightforward because the
> encoding/decoding logic is isolated in a dedicated module on both sides.

---

## Fingerprint2 UART Protocol

Source: [m5stack/M5Unit-Fingerprint2](https://github.com/m5stack/M5Unit-Fingerprint2)
(MIT license, reverse-engineered from C++ source).

The A-K323CP module is driven by the onboard STM32, which exposes a higher-level
packet-based protocol over UART. This is the only interface visible to the ESP32.

### Frame Format

```
┌──────────┬───────────┬──────────┬──────────┬────────────┬──────────┐
│ START(2) │ ADDR(4)   │ TYPE(1)  │ LEN(2)   │ DATA(n)    │ CSUM(2)  │
└──────────┴───────────┴──────────┴──────────┴────────────┴──────────┘
```

| Field     | Size    | Description                                              |
|-----------|---------|----------------------------------------------------------|
| `START`   | 2 bytes | Fixed: `0xEF01`                                          |
| `ADDR`    | 4 bytes | Module address, default `0xFFFFFFFF`                     |
| `TYPE`    | 1 byte  | Packet type (see below)                                  |
| `LEN`     | 2 bytes | `len(DATA) + 2` (accounts for the 2-byte checksum)       |
| `DATA`    | n bytes | Payload (first byte is the command code in cmd packets)  |
| `CSUM`    | 2 bytes | `(TYPE + LEN + sum(DATA)) & 0xFFFF`                      |

### Packet Types

| Value  | Name          |
|--------|---------------|
| `0x01` | Command       |
| `0x02` | Data          |
| `0x07` | ACK           |
| `0x08` | End of Data   |

### Key Commands

| Code   | Name                  | Description                                           |
|--------|-----------------------|-------------------------------------------------------|
| `0x01` | `PS_GetImage`         | Capture image (verification mode)                     |
| `0x29` | `PS_GetEnrollImage`   | Capture image (enrollment mode)                       |
| `0x02` | `PS_GenChar`          | Extract fingerprint features into buffer              |
| `0x04` | `PS_Search`           | Search library for a matching template                |
| `0x05` | `PS_RegModel`         | Merge feature buffers into a template                 |
| `0x06` | `PS_StoreChar`        | Store template to flash at given page ID              |
| `0x0C` | `PS_DeletChar`        | Delete one or more stored templates                   |
| `0x0D` | `PS_Empty`            | Wipe the entire fingerprint library                   |
| `0x31` | `PS_AutoEnroll`       | High-level enroll (module handles all steps)          |
| `0x32` | `PS_AutoIdentify`     | High-level identify (module handles all steps)        |
| `0x35` | `PS_HandShake`        | Verify the module is alive                            |
| `0x3C` | `PS_ControlBLN`       | Control the RGB LED ring                              |
| `0xD2` | `PS_SetWorkMode`      | Set sleep/active mode                                 |
| `0xD4` | `PS_ActivateModule`   | Wake the fingerprint module from sleep                |

### Wakeup Packet

When the module wakes autonomously, it emits a fixed 12-byte sequence:

```
EF 01 FF FF FF FF 07 00 03 FF 01 09
```

This packet should be detected and dispatched separately from normal ACK packets.

### LED Control Values

```
Mode  : 1=Breathing  2=Flashing  3=On  4=Off  5=FadeIn  6=FadeOut
Color : 0=Off  1=Blue  2=Green  3=Cyan  4=Red  5=Purple  6=Yellow  7=White
```

---

## Roadmap

### Phase 1 — Hardware Hello World
- Set up the Rust/ESP32 toolchain (`espup`, `esp-idf-template`).
- Blink the M5StickC PLUS2 internal LED.
- Display text on the LCD (`embedded-graphics`).

### Phase 2 — Bio-Guard
- Wire the Fingerprint2 Unit via Grove (UART at 115,200 bps).
- Implement the `fingerprint2-rs` driver:
  - Handshake and module activation.
  - `PS_AutoEnroll`: associate a finger to an ID (0–9).
  - `PS_AutoIdentify`: return the matched finger ID or an error.
- On successful match: display "Authenticated" on the LCD with the service name.

### Phase 3 — Crypto Engine
- Receive a Unix timestamp from `cyberkey-cli` over USB serial at config time; store it in RTC memory.
- Store the last-known timestamp in RTC memory to survive power cycles and light sleep.
- Integrate `totp-rs`: derive a 6-digit code from a base32 secret + timestamp.
- Store and retrieve TOTP secrets from NVS flash.

### Phase 4 — Ghost Typist (Bluetooth HID)
- Configure `esp32-nimble` as a BLE HID keyboard.
- Implement the HID Report Descriptor for a minimal keyboard profile.
- On finger match: type `[6-digit code][ENTER]` on the paired device.

### Phase 5 — CLI Configurator
- Firmware listens on UART0 at all times; CLI sessions begin automatically when the CLI
  connects and sends a `ping` command (no explicit config mode to activate).
- `cyberkey-cli` desktop binary (`dialoguer` + `tabled`) allows:
  - Listing configured finger-to-service mappings.
  - Adding a new entry (triggers enrollment + stores TOTP secret in NVS).
  - Removing an entry.
  - Testing TOTP code generation for a given entry.
  - **Factory reset**: wipe all fingerprint templates (`PS_Empty`) and erase the NVS
    partition in a single atomic operation, then reboot.
- **Physical button fallback**: holding the M5StickC PLUS2 main button for 5 seconds
  during boot triggers a factory reset without requiring CLI access. This is the recovery
  path if the firmware is in a state where the CLI cannot connect.

---

## Pre-Hardware Preparation Plan

All steps below are achievable without physical hardware.

---

### Step 1 — Toolchain Setup (est. 2–3 h, potentially tricky on macOS)

```sh
# Update rustup
rustup update

# Install espup — the ESP32 Rust toolchain manager
cargo install espup --locked

# Download and install the Xtensa toolchain
espup install
# Generates ~/export-esp.sh

# Source environment variables (add to ~/.zshrc or ~/.bashrc)
mv ~/export-esp.sh ~/.espup/export-esp.sh
. ~/.espup/export-esp.sh

# Flashing and monitoring tools
cargo install espflash --locked
cargo install cargo-espmonitor --locked

# Project scaffolding tool
cargo install cargo-generate --locked

# Required by esp-idf-sys
cargo install ldproxy --locked

# System dependencies (macOS)
brew install cmake ninja python3
```

---

### Step 2 — Workspace Scaffolding

```sh
# Create library crates
cargo new --lib crates/cyberkey-core
cargo new --lib crates/fingerprint2-rs
cargo new --bin crates/cyberkey-cli

# Generate the firmware crate from the official esp-idf template
# (select: esp32, advanced options yes, esp-idf v5.3)
cargo generate esp-rs/esp-idf-template cargo --name firmware

# Create the GitHub Actions CI workflow
mkdir -p .github/workflows
# Then create .github/workflows/ci.yml (see Testing Strategy for contents)
```

Root `Cargo.toml`:

```toml
[workspace]
members = [
    "crates/cyberkey-core",
    "crates/fingerprint2-rs",
    "crates/cyberkey-cli",
    "firmware",
]
resolver = "2"

[profile.release]
opt-level = "s"   # optimize for size on embedded targets
```

---

### Step 3 — `cyberkey-core`: TOTP Engine + Config Schema *(est. 1–2 h)*

`no_std` crate (compiled without `std` in production; tests run natively on desktop via
`cargo test`). No heap allocator required — all data lives on the stack via `heapless`.

**Why not `totp-rs`**: `totp-rs` ≥ v5 internally uses `Vec<u8>` and `String` (both
heap-allocated), making it incompatible with a true `no_std`/no-alloc crate. The feature
flag `sha1` mentioned in some notes does not exist in v5 — SHA-1 is always a plain
dependency, not a feature gate. TOTP is ~40 lines of code (HMAC-SHA1 + dynamic
truncation + base32 decode), so we implement it directly on top of the RustCrypto
primitives.

Key dependencies:

```toml
[dependencies]
hmac     = { version = "0.12", default-features = false }  # no_std HMAC (RustCrypto)
sha1     = { version = "0.10", default-features = false }  # no_std SHA-1 (RustCrypto)
heapless = "0.8"   # stack-allocated Vec/String — no allocator required
# Note: heapless 0.9.x bumped MSRV to 1.87, which is too new for the ESP-IDF toolchain.
# Stick with 0.8 until the toolchain catches up.
```

`lib.rs` gate (allows `cargo test` to run on desktop without a panic handler stub):

```rust
// Compiled as no_std in production; the test harness re-enables std automatically.
#![cfg_attr(not(test), no_std)]
```

**Module breakdown**:

| File | Contents |
|---|---|
| `error.rs` | `TotpError`, `ConfigError` |
| `config.rs` | `TotpEntry` + `TotpEntry::new` constructor, `CyberKeyConfig` + all methods |
| `totp.rs` | `generate_totp`, inline `base32_decode` |

---

**`error.rs`** — all error variants, concrete and locked in by tests:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum TotpError {
    /// secret_b32 contains a character outside the RFC 4648 base32 alphabet.
    InvalidBase32,
    /// Input exceeds 64 base32 characters — the decoded secret would overflow the
    /// 40-byte on-stack buffer.
    SecretTooLong,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    /// The config already holds 10 entries and cannot accept another.
    Full,
    /// Another entry already claims this finger_id; use remove_by_label first.
    DuplicateFingerSlot,
    /// The provided label exceeds 32 characters. The caller must truncate it.
    LabelTooLong,
    /// The provided secret_b32 exceeds 64 characters.
    SecretTooLong,
    /// No entry matched the requested label.
    EntryNotFound,
}
```

---

**`config.rs`** — schema + methods:

```rust
pub struct TotpEntry {
    pub finger_id:  u8,          // 0–9, fingerprint sensor slot
    pub label:      String<32>,  // e.g. "GitHub", "AWS"
    pub secret_b32: String<64>,  // base32-encoded TOTP secret (stored undecoded)
}
```

`TotpEntry::new(finger_id, label: &str, secret_b32: &str) -> Result<TotpEntry, ConfigError>`
— convenience constructor that validates lengths and converts to `heapless::String`.

```rust
pub struct CyberKeyConfig {
    pub entries: heapless::Vec<TotpEntry, 10>,
}
```

`CyberKeyConfig` methods:

| Method | Description |
|---|---|
| `new() -> Self` | Returns an empty config |
| `add_entry(entry) -> Result<(), ConfigError>` | Validates uniqueness + capacity, then pushes |
| `remove_by_label(label) -> Result<(), ConfigError>` | Order-preserving removal of first label match |
| `find_by_finger_id(id) -> Option<&TotpEntry>` | Linear scan (n ≤ 10, constant in practice) |
| `iter() -> impl Iterator<Item = &TotpEntry>` | Insertion-order iteration |

**Behavioural decisions** (locked in by tests, not open questions):

- **Duplicate `finger_id`** → `Err(ConfigError::DuplicateFingerSlot)`. A slot
  reassignment must be an explicit `remove_by_label` + `add_entry`; silent overwrite is
  never acceptable on a security device.
- **Label > 32 chars** → `Err(ConfigError::LabelTooLong)`. The CLI layer is responsible
  for truncating before calling; the core never silently drops characters.
- **Secret > 64 chars** → `Err(ConfigError::SecretTooLong)`.

---

**`totp.rs`** — the algorithm:

TOTP (RFC 6238 / RFC 4226) in three steps:
1. Base32-decode `secret_b32` into a fixed `[u8; 40]` stack buffer
   (max 64 base32 input chars → max 40 raw bytes).
2. HMAC-SHA1 over the 8-byte big-endian HOTP counter `T = floor(timestamp / 30)`.
3. Dynamic truncation: `offset = mac[19] & 0x0f`;
   `code = u32::from_be_bytes([mac[o]&0x7f, mac[o+1], mac[o+2], mac[o+3]]) % 1_000_000`.

```rust
// Core function: derive a 6-digit TOTP code.
// secret_b32 : base32-encoded key, e.g. "JBSWY3DPEHPK3PXP" (case-insensitive)
// timestamp  : Unix timestamp in seconds (from RTC or USB clock sync)
pub fn generate_totp(secret_b32: &str, timestamp: u64) -> Result<u32, TotpError>;
```

---

Unit tests — all behaviours pinned before firmware integration:

```rust
// "JBSWY3DPEHPK3PXP" decodes to b"Hello!\xDE\xAD\xBE\xEF" (10 raw bytes).
// counter = floor(59 / 30) = 1. Correct 6-digit TOTP for this key/counter pair: 996554.
#[test]
fn totp_known_vector_t59() {
    assert_eq!(generate_totp("JBSWY3DPEHPK3PXP", 59).unwrap(), 996554);
}

// RFC 6238 Appendix B uses key "12345678901234567890" (base32: "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").
// At t=59 the RFC mandates an 8-digit code of 94287082; our 6-digit output = 94287082 % 10^6 = 287082.
#[test]
fn totp_rfc6238_vector_t59() {
    assert_eq!(
        generate_totp("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59).unwrap(),
        287082,
    );
}

// All timestamps in the same 30-second window must yield the same code.
// Window 2 spans [60, 89]; window 3 spans [90, 119] — must differ.
#[test]
fn totp_window_stability() { ... }

// Lower-case secrets must produce identical codes to upper-case (RFC 4648 is
// case-insensitive; authenticator apps emit uppercase but some tools use lowercase).
#[test]
fn totp_case_insensitive_secret() { ... }

// RFC 4648 padding ('=') must be silently ignored — result identical to unpadded form.
#[test]
fn totp_padding_ignored() {
    // "JBSWY3DPEHPK3PXP======" must give the same 996554 as the unpadded form.
    assert_eq!(generate_totp("JBSWY3DPEHPK3PXP======", 59).unwrap(), 996554);
}

// Characters outside A-Z and 2-7 must be rejected immediately.
#[test]
fn totp_invalid_base32_returns_err() {
    assert_eq!(generate_totp("!!!INVALID!!!", 59), Err(TotpError::InvalidBase32));
}

// '0' and '1' are not in the base32 alphabet (often confused with 'O' and 'I').
#[test]
fn totp_digit_zero_and_one_are_invalid() { ... }

// 65 base32 characters → exceeds 64-char limit → SecretTooLong (buffer-overflow guard).
#[test]
fn totp_secret_too_long_returns_err() { ... }

// 64 base32 characters decode to exactly 40 bytes — must succeed without error.
#[test]
fn totp_exactly_64_char_secret_accepted() { ... }

// Pushing an 11th entry must fail — never silently drop data.
#[test]
fn config_capacity_at_limit() { ... }

// Two entries with the same finger_id must be rejected.
#[test]
fn config_duplicate_finger_slot_rejected() { ... }

// 32-char label accepted; 33-char label returns LabelTooLong.
#[test]
fn config_label_boundary() { ... }

// remove_by_label happy path + EntryNotFound on second attempt.
#[test]
fn config_remove_by_label() { ... }

// find_by_finger_id returns Some for known id, None for unknown.
#[test]
fn config_find_by_finger_id() { ... }
```

---

### Step 4 — `fingerprint2-rs`: UART Driver *(est. 2–3 h)*

`no_std` crate, generic over any `embedded-hal-nb` UART implementation. Can be tested on
desktop using a mock UART backed by an in-memory buffer.

> **embedded-hal v0.2 vs v1.0**: `embedded-hal` released a major breaking v1.0 in
> January 2024. The serial/UART traits are no longer in `embedded-hal` itself — they
> moved to a companion crate, `embedded-hal-nb` (nb = non-blocking, using `nb::Result`).
> `esp-idf-hal` >= 0.43 targets v1.0. Many tutorials and crates found online still
> reference the old `embedded_hal::serial::Read/Write` (v0.2) API — these must be
> ignored. Always check the `esp-idf-hal` changelog to confirm which version of
> `embedded-hal` is in use before writing any driver code.

Key dependencies:

```toml
[dependencies]
embedded-hal    = "1"
embedded-hal-nb = "1"   # non-blocking serial traits (Read/Write over UART)
nb              = "1"   # nb::Result / nb::Error::WouldBlock
heapless        = "0.8"
```

`lib.rs` gate (same pattern as `cyberkey-core`):

```rust
// Compiled as no_std in production; the test harness re-enables std automatically.
#![cfg_attr(not(test), no_std)]
```

Internal module layout:

```
fingerprint2-rs/src/
├── lib.rs          — public re-exports
├── packet.rs       — frame serialization / deserialization
├── commands.rs     — typed wrappers for each command
├── driver.rs       — Fingerprint2Driver<UART> struct
└── error.rs        — FingerprintError<E> enum
```

**Module breakdown**:

| File | Contents |
|---|---|
| `error.rs` | `FingerprintError<E>` — all driver-level error variants |
| `packet.rs` | `PacketType`, `Frame`, `serialize`, `deserialize`, `is_wakeup_packet` |
| `commands.rs` | Opcode constants, `AutoEnrollFlags`, `LedMode`, `LedColor` |
| `driver.rs` | `Fingerprint2Driver<UART>` — blocking public API + `DriverEvent` |

---

**`error.rs`** — all error variants, concrete and locked in by tests:

```rust
#[derive(Debug)]
pub enum FingerprintError<E> {
    /// Received frame begins with wrong magic bytes (expected 0xEF01).
    BadFrame,
    /// Received frame has a checksum mismatch.
    BadChecksum,
    /// No bytes arrived within the polling window.
    /// In tests: MockUart rx buffer exhausted before a full frame was assembled.
    Timeout,
    /// PS_AutoIdentify returned a "no match" confirmation code.
    NoMatch,
    /// PS_AutoEnroll failed (poor image quality, repeated low-area reads, etc.).
    EnrollFailed,
    /// Sensor returned a non-zero confirmation code not otherwise mapped.
    /// The raw confirmation byte is preserved for diagnostics.
    SensorError(u8),
    /// Underlying UART read or write error.
    Uart(E),
}
```

---

**`packet.rs`** — frame types and codec:

```rust
pub const FRAME_MAGIC:  u16   = 0xEF01;
pub const DEFAULT_ADDR: u32   = 0xFFFF_FFFF;
pub const MAX_DATA_LEN: usize = 64;     // max payload bytes per frame

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PacketType {
    Command   = 0x01,
    Data      = 0x02,
    Ack       = 0x07,
    EndOfData = 0x08,
}

pub struct Frame {
    pub addr:        u32,
    pub packet_type: PacketType,
    /// Payload bytes; for ACK packets, DATA[0] is the sensor's confirmation code.
    pub data:        heapless::Vec<u8, MAX_DATA_LEN>,
}

/// Serialise a Frame into a caller-supplied byte buffer.
/// Returns the number of bytes written, or None if the buffer is too small.
pub fn serialize(frame: &Frame, buf: &mut [u8]) -> Option<usize>;

/// Deserialise a Frame from a raw byte slice.
/// Returns Err(BadFrame) on wrong magic or truncated input,
/// Err(BadChecksum) on checksum mismatch.
pub fn deserialize(buf: &[u8]) -> Result<Frame, FingerprintError<core::convert::Infallible>>;

/// Returns true iff buf exactly matches the 12-byte autonomous wakeup sequence:
/// EF 01 FF FF FF FF 07 00 03 FF 01 09
pub fn is_wakeup_packet(buf: &[u8]) -> bool;
```

Checksum formula (from protocol spec):

```
checksum = (TYPE as u32 + LEN as u32 + DATA.iter().map(|b| *b as u32).sum::<u32>()) & 0xFFFF
```

The 2-byte `LEN` field encodes `len(DATA) + 2` (the 2-byte checksum is counted in the length).  
Minimum valid frame size: 2 (START) + 4 (ADDR) + 1 (TYPE) + 2 (LEN) + 0 (DATA) + 2 (CSUM) = **11 bytes**.

---

**`commands.rs`** — opcodes and typed parameter types:

```rust
// Opcode constants — first byte of DATA in a Command frame.
// Names preserved verbatim from the M5Stack STM32 firmware for traceability.
pub const PS_GET_IMAGE:     u8 = 0x01;
pub const PS_GEN_CHAR:      u8 = 0x02;
pub const PS_SEARCH:        u8 = 0x04;
pub const PS_REG_MODEL:     u8 = 0x05;
pub const PS_STORE_CHAR:    u8 = 0x06;
pub const PS_DELET_CHAR:    u8 = 0x0C;   // typo from upstream preserved
pub const PS_EMPTY:         u8 = 0x0D;
pub const PS_AUTO_ENROLL:   u8 = 0x31;
pub const PS_AUTO_IDENTIFY: u8 = 0x32;
pub const PS_HANDSHAKE:     u8 = 0x35;
pub const PS_CONTROL_BLN:   u8 = 0x3C;
pub const PS_SET_WORK_MODE: u8 = 0xD2;
pub const PS_ACTIVATE:      u8 = 0xD4;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LedMode {
    Breathing = 1,
    Flashing  = 2,
    On        = 3,
    Off       = 4,
    FadeIn    = 5,
    FadeOut   = 6,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LedColor {
    Off    = 0,
    Blue   = 1,
    Green  = 2,
    Cyan   = 3,
    Red    = 4,
    Purple = 5,
    Yellow = 6,
    White  = 7,
}

/// Flags byte for PS_AutoEnroll.
/// Bit 0: overwrite an existing template at the target page ID (0 = reject if occupied).
/// All other bits reserved — must be zero.
pub struct AutoEnrollFlags {
    pub allow_overwrite: bool,
}

impl AutoEnrollFlags {
    pub fn as_byte(&self) -> u8 {
        self.allow_overwrite as u8
    }
}
```

> **Opcode naming**: `PS_DELET_CHAR` preserves the upstream typo (`DeletChar`, not
> `DeleteChar`) so that `grep PS_DeletChar` in the M5Stack C++ source maps directly to
> the Rust constant.

Driver skeleton:

```rust
pub struct Fingerprint2Driver<UART> {
    uart:    UART,
    address: u32,   // default 0xFFFF_FFFF
}

impl<UART> Fingerprint2Driver<UART>
where
    UART: embedded_hal_nb::serial::Read<u8>
        + embedded_hal_nb::serial::Write<u8>,
{
    pub fn new(uart: UART) -> Self { ... }

    /// Verify the module is responsive.
    pub fn handshake(&mut self) -> Result<(), FingerprintError<UART::Error>>;

    /// High-level enrollment: the module manages all capture steps internally.
    pub fn auto_enroll(
        &mut self,
        id: u16,
        count: u8,
        flags: AutoEnrollFlags,
    ) -> Result<(), FingerprintError<UART::Error>>;

    /// High-level identification: returns the matched page ID.
    pub fn auto_identify(
        &mut self,
        security_level: u8,
    ) -> Result<u16, FingerprintError<UART::Error>>;

    /// Control the RGB LED ring.
    pub fn set_led(
        &mut self,
        mode: LedMode,
        color: LedColor,
        loops: u8,
    ) -> Result<(), FingerprintError<UART::Error>>;

    /// Delete one or more stored templates starting at page_id.
    pub fn delete_template(
        &mut self,
        page_id: u16,
        count: u16,
    ) -> Result<(), FingerprintError<UART::Error>>;

    /// Poll for an incoming unsolicited frame (non-blocking).
    /// Returns Ok(DriverEvent::Wakeup) when the sensor wakes autonomously.
    /// Returns Err(nb::Error::WouldBlock) immediately when no data is available.
    pub fn poll_event(
        &mut self,
    ) -> nb::Result<DriverEvent, FingerprintError<UART::Error>>;
}

/// Events returned by poll_event().
pub enum DriverEvent {
    /// Sensor woke autonomously (finger placed on pad). Call auto_identify next.
    Wakeup,
    /// An unsolicited ACK frame arrived (uncommon; captured for diagnostics).
    Ack { confirm: u8 },
}
```

**Behavioural decisions** (locked in by tests, not open questions):

- **Blocking vs. nb**: the main API (`handshake`, `auto_enroll`, `auto_identify`,
  `set_led`, `delete_template`) is synchronous — internally, `nb::block!()` loops until
  each byte arrives. `poll_event` is the single non-blocking method; it returns
  `nb::Error::WouldBlock` immediately when `rx` is empty, allowing the caller to sleep
  between polls without spinning.
- **Timeout simulation in tests**: `MockUart` does not implement a real timer. Tests that
  validate `Timeout` behaviour use a `LimitedMockUart` wrapper that returns `WouldBlock`
  a fixed number of times before returning `Err(FingerprintError::Timeout)`, simulating
  the real driver's internal retry cap.
- **Wakeup packet priority**: `is_wakeup_packet()` is checked on the first 12 bytes of
  any incoming frame before any other parsing. If it matches, the bytes are consumed and
  `DriverEvent::Wakeup` is returned — checksum validation is intentionally skipped for
  this fixed sequence.
- **Single ACK per command**: the driver reads exactly one ACK frame per outgoing
  command. Multi-packet transfers (e.g. raw image upload) are out of scope before v1.0.
- **Confirmation code mapping**: any ACK with `DATA[0] != 0x00` is mapped as follows:
  - `0x09` (library empty or no matching template) → `NoMatch`
  - `0x03`, `0x06`, `0x07`, `0x0A` (enrollment failures: bad quality, small area, poor
    points, merge failed) → `EnrollFailed`
  - All other non-zero codes → `SensorError(code)` (raw byte preserved for diagnostics)

#### Testing `fingerprint2-rs` without hardware

Because the driver is generic over any `embedded_hal` UART implementation, a `MockUart`
struct can substitute the real peripheral in all unit tests:

```rust
use std::collections::VecDeque;

struct MockUart {
    rx: VecDeque<u8>,  // bytes the "sensor" will return
    tx: Vec<u8>,       // bytes the driver actually sent — inspectable after the call
}

impl embedded_hal_nb::serial::Read<u8> for MockUart {
    type Error = core::convert::Infallible;
    fn read(&mut self) -> nb::Result<u8, Self::Error> {
        self.rx.pop_front().ok_or(nb::Error::WouldBlock)
    }
}

impl embedded_hal_nb::serial::Write<u8> for MockUart {
    type Error = core::convert::Infallible;
    fn write(&mut self, word: u8) -> nb::Result<(), Self::Error> {
        self.tx.push(word); Ok(())
    }
    fn flush(&mut self) -> nb::Result<(), Self::Error> { Ok(()) }
}
```

> `MockUart` lives in `#[cfg(test)]` inside `fingerprint2-rs` — it is never compiled
> into the firmware binary.

Suggested unit tests for `packet.rs`:

- **Checksum round-trip**: serialize a known command frame, recompute
  `(TYPE + LEN + sum(DATA)) & 0xFFFF`, assert it matches the last two bytes.
- **Frame round-trip**: `serialize(frame)` → `deserialize(bytes)` → assert field equality
  for `START`, `ADDR`, `TYPE`, `LEN`, `DATA`.
- **Wrong magic bytes**: feed `0xEF02 …` instead of `0xEF01 …`, expect
  `Err(FingerprintError::BadFrame)`.
- **Checksum mismatch**: flip one byte in a valid serialized frame, expect
  `Err(FingerprintError::BadChecksum)`.
- **Wakeup packet detection**: feed the exact 12-byte sequence
  `EF 01 FF FF FF FF 07 00 03 FF 01 09` and verify it is dispatched as a
  `WakeupEvent` variant rather than treated as a standard ACK.
- **Truncated frame**: feed only 7 bytes (below the 11-byte minimum), expect
  `Err(FingerprintError::BadFrame)`.
- **LEN field mismatch**: `LEN` claims 10 DATA bytes but only 5 bytes follow, expect
  `Err(FingerprintError::BadFrame)`.
- **Non-default address preserved**: `serialize` + `deserialize` a frame with
  `addr = 0xABCD_1234`, assert the address round-trips without corruption.

Unit tests for `commands.rs`:

- **`LedColor` repr values**: `assert_eq!(LedColor::Blue as u8, 1)` and
  `assert_eq!(LedColor::Red as u8, 4)` — sentinel check against the protocol datasheet.
- **`LedMode` repr values**: `assert_eq!(LedMode::Breathing as u8, 1)` and
  `assert_eq!(LedMode::Off as u8, 4)`.
- **`AutoEnrollFlags::as_byte` false**: `AutoEnrollFlags { allow_overwrite: false }.as_byte() == 0x00`.
- **`AutoEnrollFlags::as_byte` true**: `AutoEnrollFlags { allow_overwrite: true }.as_byte() == 0x01`.
- **`is_wakeup_packet` true**: the exact 12-byte wakeup sequence returns `true`.
- **`is_wakeup_packet` false on normal ACK**: a valid `handshake` success ACK byte
  sequence returns `false`.
- **`is_wakeup_packet` false on short slice**: a 4-byte slice returns `false` without
  panicking.

Suggested integration-level tests using `MockUart`:

- **`handshake` happy path**: pre-load the expected ACK response bytes into `rx`, call
  `driver.handshake()`, assert `Ok(())`, then inspect `tx` to confirm the correct command
  bytes were sent.
- **`handshake` no response**: leave `rx` empty, expect
  `Err(FingerprintError::Timeout)` (or appropriate `WouldBlock` propagation).
- **`auto_identify` no match**: pre-load a "no finger found" ACK payload, expect
  `Err(FingerprintError::NoMatch)`.
- **`set_led` encoding**: call `set_led(LedMode::Breathing, LedColor::Blue, 3)`, inspect
  `tx` and assert the correct `PS_ControlBLN` opcode and parameter bytes were emitted.
- **`auto_enroll` happy path**: pre-load a multi-ACK sequence (one `0x00` ACK per capture
  pass; for `count=3`, three consecutive ACK frames), assert `Ok(())`, and inspect `tx`
  for the `PS_AUTO_ENROLL` opcode followed by the correct `id` and `count` bytes.
- **`auto_enroll` quality failure**: pre-load a single ACK with confirmation code `0x06`
  (image too noisy), expect `Err(FingerprintError::EnrollFailed)`.
- **`auto_identify` successful match**: pre-load an ACK with `DATA[0] = 0x00` followed by
  `[page_id_hi, page_id_lo, score_hi, score_lo]`, assert `Ok(page_id)` equals the
  expected value.
- **`delete_template` byte encoding**: pre-load ACK `0x00`, call `delete_template(5, 1)`,
  inspect `tx` for the `PS_DELET_CHAR` opcode and the correct page ID and count bytes.
- **Unmapped sensor error propagation**: pre-load ACK with confirmation code `0x15`
  (wrong password), expect `Err(FingerprintError::SensorError(0x15))`.
- **`poll_event` wakeup**: pre-load the 12-byte wakeup sequence into `rx`, call
  `driver.poll_event()`, assert `Ok(DriverEvent::Wakeup)`.
- **`poll_event` no data**: leave `rx` empty, call `driver.poll_event()`, assert
  `Err(nb::Error::WouldBlock)`.

---

### Step 5 — `cyberkey-cli`: Desktop Configuration Tool

Standard `std` binary. Communicates with the firmware over USB serial.

Key dependencies:

```toml
[dependencies]
dialoguer      = "0.11"
tabled         = "0.17"
serialport     = "4"
serde          = { version = "1", features = ["derive"] }
serde_json     = "1"
clap           = { version = "4", features = ["derive"] }
cyberkey-core  = { path = "../cyberkey-core" }
anyhow         = "1"
```

Expected interaction flow:

```
$ cyberkey-cli

Connected to CyberKey on /dev/tty.usbserial-3 (firmware v0.1.0)

? What do you want to do?
  > List configured fingers
    Add a new finger
    Remove a finger
    Test TOTP generation
    Sync device clock
    Factory reset
    Exit

[List configured fingers]
┌──────┬──────────────┬────────────────────────────┐
│ Slot │ Service      │ Secret (masked)            │
├──────┼──────────────┼────────────────────────────┤
│  0   │ GitHub       │ JBSW********************   │
│  1   │ AWS          │ OJA3********************   │
│  2   │ VPN corp     │ K5QW********************   │
└──────┴──────────────┴────────────────────────────┘

Slot numbers are internal identifiers assigned automatically — they are not meaningful
to the user. The associated finger is the only selector at authentication time.

[Add a new finger]
  Service name: GitHub
  TOTP secret (base32): **********************
  → Slot 0 assigned automatically (next available).
  → Place your finger on the sensor when ready...
     [1/3] Place finger      ████░░░░░░ LED: breathing blue
     [1/3] Lift finger       ████░░░░░░
     [2/3] Place finger      ████████░░
     [2/3] Lift finger       ████████░░
     [3/3] Place finger      ██████████
  ✓ Enrollment successful. "GitHub" bound to slot 0.

  If enrollment fails (poor quality, finger too dry/wet), the CLI offers up to
  3 application-level retries before aborting. The partially enrolled template is
  cleaned up on the sensor before each retry (PS_DeletChar on the assigned slot).

  Atomicity guarantee: if enrollment succeeds on the sensor but the subsequent NVS
  write fails, the firmware issues PS_DeletChar to remove the orphaned template before
  returning an error to the CLI. The device is never left in a state where a finger is
  recognised but has no associated TOTP secret.
```

#### Testing `cyberkey-cli`

The CLI binary delegates all TOTP logic to `cyberkey-core`, so cryptographic correctness
is already covered upstream. The testable surface specific to `cyberkey-cli` is the serial
protocol layer — the encoding and decoding of commands exchanged with the firmware over
USB serial.

Suggested unit tests:

- **Command serialization**: given a structured command (e.g.
  `AddEntryCommand { label: "GitHub", secret_b32: "JBSWY3…" }` — note: no `finger_id`,
  slot is auto-assigned by firmware), verify that the serialized byte payload matches
  the expected format.
- **Response deserialization**: given a raw byte slice representing a known firmware
  response, assert that the parsed struct matches expected field values.
- **Secret masking**: the display helper that masks secrets for the table output
  (e.g. `"JBSWY3DPEHPK3PXP"` → `"JBSW********************"`) should be unit-tested for
  edge cases: empty secret, secret shorter than the visible prefix, exact prefix length.
- **Clock sync payload**: the Unix timestamp sent by "Sync device clock" should be
  serialized and deserialized correctly, including boundary values (t = 0, u64::MAX).

The factory reset command requires an explicit confirmation step in the CLI before the
request is sent to the firmware, to prevent accidental data loss:

```
! This will permanently erase all fingerprints and TOTP secrets.
  Type "RESET" to confirm: _
```

The corresponding wire protocol exchange:

```
→ {"cmd":"factory_reset","confirm":"RESET"}
← {"ok":true}   ← device reboots immediately after this response
```

> The actual serial port (`serialport` crate) does not need to be mocked in unit tests.
> Encoding and decoding logic should be isolated in pure functions accepting `&[u8]` /
> `impl Write` parameters, keeping them testable without a connected device.

---

### Step 6 — BLE HID: Risk Mitigation

BLE HID keyboard on ESP32 with Rust is the highest-risk component of the project.
No turnkey crate exists; the HID Report Descriptor must be written manually.

Reference implementation path: `esp32-nimble` → custom GATT service → HID profile.

The HID Report Descriptor for a minimal keyboard (to be validated against macOS and
Windows pairing):

```rust
pub const HID_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01,  // Usage Page (Generic Desktop)
    0x09, 0x06,  // Usage (Keyboard)
    0xA1, 0x01,  // Collection (Application)
    // Modifier byte (Ctrl, Shift, Alt, GUI)
    0x05, 0x07,  0x19, 0xE0,  0x29, 0xE7,
    0x15, 0x00,  0x25, 0x01,  0x75, 0x01,  0x95, 0x08,  0x81, 0x02,
    // Reserved byte
    0x95, 0x01,  0x75, 0x08,  0x81, 0x01,
    // Key array (6 simultaneous keys)
    0x95, 0x06,  0x75, 0x08,  0x15, 0x00,  0x25, 0x65,
    0x05, 0x07,  0x19, 0x00,  0x29, 0x65,  0x81, 0x00,
    0xC0,        // End Collection
];
```

> **Open question**: Classic Bluetooth BR/EDR HID has broader OS compatibility than BLE
> HID, especially on older macOS and Windows versions. The ESP32-PICO-V3-02 supports
> both. The final choice between BLE HID and Classic BT HID should be evaluated once
> hardware is available and pairing tests can be performed.

> **BLE pairing security**: the device types TOTP codes wirelessly — a rogue BLE host
> that pairs with the device would silently receive authentication codes. The pairing
> implementation must enforce:
> - **LE Secure Connections** (LESC) with MITM protection, using `esp32-nimble`'s
>   `set_security` API (`BLE_SM_IO_CAP_DISP_ONLY` + `BLE_SM_PAIR_AUTHREQ_MITM`).
> - **Single paired host**: the device should only maintain one bonded host at a time.
>   Adding a second host must require a factory reset or an explicit re-pairing procedure
>   initiated from the CLI.
> - **Pairing window**: the device should only accept new pairing requests when explicitly
>   unlocked via the CLI (`{"cmd":"allow_pairing"}`), not at all times.
> These constraints must be validated against macOS and Windows during Phase 4 testing.

---

### Step 7 — Background Reading

Recommended reading order, calibrated to this stack:

1. [The Rust on ESP Book](https://docs.esp-rs.org/book/) — chapters 1–4
   (toolchain setup, Hello World, peripheral access model)
2. [`esp-idf-hal` examples](https://github.com/esp-rs/esp-idf-hal/tree/master/examples)
   — focus on `uart.rs`, `gpio.rs`, `nvs.rs`
3. [`esp32-nimble` examples](https://github.com/taks/esp32-nimble)
   — `ble_keyboard` is the BLE HID reference
4. [RFC 6238](https://datatracker.ietf.org/doc/html/rfc6238) — TOTP specification
5. [USB HID Usage Tables 1.12](https://www.usb.org/sites/default/files/documents/hut1_12v2.pdf)
   — §10 (Keyboard page), needed for the HID descriptor

---

## Deliverables Before Hardware Arrival

| Deliverable                                      | Verifiable without hardware |
|--------------------------------------------------|-----------------------------|
| ESP32 Rust toolchain installed                   | ✅ `espflash --version`      |
| Cargo workspace scaffolded                       | ✅ `cargo build --workspace` |
| `cyberkey-core` with passing TOTP tests          | ✅ `cargo test`              |
| `fingerprint2-rs` packet codec + mock UART tests | ✅ `cargo test`              |
| `cyberkey-cli` compiles with menu skeleton       | ✅ `cargo build`             |
| HID Report Descriptor constant defined           | ✅                           |
| `docs/devlog.md` initialized                     | ✅                           |
| GitHub Actions CI green (`cargo test --workspace`)| ✅ push / PR to `main`      |

---

## Risks & Open Questions

### High — BLE HID implementation complexity

No ready-made Rust BLE HID keyboard crate exists for ESP32. The implementation requires
manual HID descriptor authoring and low-level NimBLE GATT configuration. Estimated
additional effort: 1–2 weeks. Mitigation: prototype the BLE keyboard independently before
integrating with the rest of the firmware.

### Medium — RTC time drift

TOTP is time-sensitive (±30 s window). The ESP32 internal RTC drifts over time and resets
on power-off. Chosen strategy: `cyberkey-cli` sends the current Unix timestamp over USB
serial at each config session ("Sync device clock" menu option); the firmware stores it in
RTC memory and uses it as the reference for all TOTP generation.

Residual drift options if longer autonomy is needed:
- Re-sync via CLI before each use (zero extra hardware, recommended default).
- Add an external RTC module (DS3231, I²C) for persistent timekeeping across power cycles
  — recommended if the device is used for long periods without connecting to a computer.

### Medium — NVS encryption Rust API availability

ESP-IDF supports encrypted NVS partitions via `nvs_flash_secure_init()`. The encryption
key (AES-256 XTS) is generated on first boot and burned into the chip's eFuses — which
are one-time programmable and cannot be read back once locked, making flash dumps
unreadable without physical decapping.

The intended boot sequence is:
1. On first boot, ESP-IDF generates a random NVS key and writes it to a dedicated eFuse
   block.
2. All subsequent NVS reads and writes go through the encrypted partition; the plaintext
   never appears on the flash bus.
3. If the eFuse block is locked (`espefuse.py burn_key_digest --no-protect-key` is
   intentionally *not* used — the protect flag must be set), the key is hardware-bound
   and irrecoverable.

**Risk**: the `esp-idf-svc` Rust crate currently exposes `EspNvs` backed by
`nvs_flash_init()`. It is not confirmed that `nvs_flash_secure_init()` is wrapped at the
Rust level. If not, the fallback is an `unsafe` call to the raw C API — feasible, but
must be verified before starting Phase 3. If the secure init path is unavailable or
proves too complex for v0.1, storing secrets in a standard NVS partition is acceptable as
a temporary measure provided the threat model is documented (physical flash dump would
expose secrets).

### Medium — No existing Rust driver for the Fingerprint2 unit

The `fingerprint2-rs` driver must be written from scratch. The protocol is well-documented
(sourced from M5Stack's official C++ library), so the risk is effort, not unknowns.

### Low — `no_std` dependency compatibility with embedded-hal 1.0

Before writing any driver code, all `no_std` dependencies chosen for `fingerprint2-rs`
and `cyberkey-core` must be verified against embedded-hal 1.0. The ecosystem is mid-
migration: many crates still only support 0.2, and some publish separate `0.x` and `1.x`
compatible versions. Crates to audit before starting:

- `embedded-hal-nb` (v1): confirms the non-blocking serial trait API.
- `heapless` (v0.8): no embedded-hal dependency, safe.
- `serde-json-core`: verify `no_std` support and that it does not pull in a conflicting
  embedded-hal version transitively.
- Any future HAL utility crate added to `fingerprint2-rs`.

If a required crate only supports 0.2, a shim crate (`embedded-hal-compat`) exists to
bridge the two versions, at the cost of added complexity.

### Medium — Factory reset is irreversible

The factory reset sequence calls `PS_Empty` on the fingerprint sensor, which wipes all
stored templates from the sensor's internal flash, and then erases the NVS partition,
removing all TOTP secrets. Both operations are permanent and cannot be undone.

Consequences:
- All finger-to-service mappings are lost. Re-enrollment is required for every entry.
- TOTP secrets must be re-entered via the CLI. If the original base32 secrets were not
  backed up externally, the corresponding accounts will require a recovery flow with
  each service provider.

Mitigations to implement:
- The CLI confirmation step (`"RESET"` typed manually) guards against accidental triggers.
- The physical button reset should require a second confirmation signal (e.g. release and
  press again within 3 seconds after the LED turns red) to reduce the risk of an
  accidental boot-time reset.
- **Recommendation**: document clearly in the user guide that TOTP secrets should be
  backed up (e.g. stored in a password manager) at enrollment time.

### Low — Grove UART pin assignment on M5StickC PLUS2

The exact GPIO numbers for the Grove port must be confirmed against the hardware schematic
before writing the UART initialization code. Assumed: G32 (TX) / G33 (RX), but this should
be verified.