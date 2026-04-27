# CyberKey

Touch an enrolled finger. A TOTP code is typed into the focused field over Bluetooth — no phone, no app, no copy-paste.

Built on the **M5StickC Plus 2** (ESP32). Pairs with macOS, Windows, and Linux as a standard BLE HID keyboard.

<!-- demo gif here -->

---

## How it works

```
┌──────────────────────────────────────────────┐
│             M5StickC Plus 2                  │
│                                              │
│  Fingerprint sensor  ──→  match slot N       │
│  NVS (AES-256-XTS)   ──→  load secret[N]     │
│  RTC (BM8563)        ──→  current timestamp  │
│                           ↓                  │
│                        TOTP code             │
│                           ↓                  │
│  NimBLE HID keyboard ──→  type it            │
└──────────────────────────────────────────────┘
                  │ BLE
                  ▼
           Host computer
```

One finger = one service. Place the enrolled finger, the device matches it, generates the 6-digit code, and types it via BLE. Wrong finger → red LED, nothing typed.

Configuration (enrollment, clock sync, bond management) happens over USB-C serial using the `cyberkey-cli` desktop tool.

---

## Hardware

| Part | Reference |
|------|-----------|
| M5StickC Plus 2 | [M5Stack SKU:K016-P2](https://shop.m5stack.com/products/m5stickc-plus2-esp32-mini-iot-development-kit) |
| Fingerprint sensor | [M5Stack Unit Fingerprint2 SKU:U203](https://shop.m5stack.com/products/fingerprint-2-unit-a-k323cp) |

The sensor connects to the M5StickC Plus 2 Grove port (UART, no soldering).

<!-- hardware photo here -->

---

## Security model

**BLE pairing** uses LESC (ECDH P-256) + MITM passkey entry. A random 6-digit passkey is generated at boot and displayed on the LCD. The host must enter it to complete pairing — a rogue device nearby cannot pair silently.

**Storage** uses ESP-IDF encrypted NVS (AES-256-XTS). The encryption key is generated at first boot and burned into the ESP32's eFuses (one-time programmable, cannot be read back). A stolen device with a dumped flash image yields only ciphertext.

**Authentication** gates the USB CLI behind fingerprint unlock. TOTP secrets are never sent over the wire in full.

Full details: [docs/ble-security.md](docs/ble-security.md) · [docs/storage.md](docs/storage.md)

---

## Build & flash

### Prerequisites

```sh
# Install the Xtensa toolchain (one-time)
cargo install espup
espup install
source ~/.espup/export-esp.sh
```

[`espflash`](https://github.com/esp-rs/espflash) is required to flash. It is pulled in automatically via `cargo run`.

### Firmware

```sh
cd firmware
cargo build --release        # build only
cargo run --release          # build, flash, and open serial monitor
```

### Desktop CLI

```sh
cargo build --release --package cyberkey-cli
# binary: target/release/cyberkey-cli
```

Connect the device over USB-C, then run `cyberkey-cli`. It auto-detects the serial port, syncs the clock, and shows an interactive menu.

---

## Tests

The `no_std` crates are fully testable on a standard Rust toolchain (no hardware needed):

```sh
cargo test --exclude firmware
```

The firmware crate targets Xtensa ESP32 and requires the Espressif toolchain; it is excluded from the above. See [docs/testing.md](docs/testing.md) for the manual smoke test checklist.

---

## Repository layout

| Crate | Target | Description |
|-------|--------|-------------|
| `crates/cyberkey-core` | `no_std` | TOTP engine (RFC 6238), config schema |
| `crates/cyberkey-hid` | `no_std` | ASCII → HID keycode table |
| `crates/fingerprint2-rs` | `no_std` | Fingerprint2 sensor UART driver |
| `crates/cyberkey-cli` | `std` | Desktop configuration tool |
| `firmware` | ESP32 only | Hardware integration, BLE, main loop |

Architecture and design decisions: [ARCHITECTURE.md](ARCHITECTURE.md)
