# CyberKey — Architecture Overview

CyberKey is a Bluetooth HID keyboard device built on the **M5StickC Plus 2** (ESP32). It stores TOTP secrets per-finger: touch the enrolled finger to the sensor, the device matches it, generates the current OTP, and types it via BLE as if you were at a keyboard.

No phone, no app, no network at authentication time.

---

## System Diagram

```
┌──────────────────────────────────────────────────────────┐
│                  M5StickC Plus 2 (ESP32)                 │
│                                                          │
│  main loop (app::run)             CLI task               │
│  ├─ button polling                (FreeRTOS thread)      │
│  ├─ fingerprint polling           │                      │
│  ├─ BLE state + HID typing        └─ JSON/UART0 ─────────┼──→ USB-C
│  └─ display updates                                      │
│                                                          │
│  NimBLE stack  ──────────────────────────────────────────┼──→ BLE
│  NVS (AES-256-XTS, eFuse key) ─ TOTP secrets            │
│  BM8563 RTC (I2C) ─ timekeeping                          │
│  ST7789V2 LCD (SPI)                                      │
│  Fingerprint2 sensor (UART1)                             │
└──────────────────────────────────────────────────────────┘
             │ BLE (HID keyboard, up to 3 links)
             ▼
      ┌──────────────┬──────────────┐
      ▼              ▼              ▼
Host computer 1 Host computer 2 Host computer 3

             │ USB-C serial (115.2k bps)
             ▼
      cyberkey-cli (desktop binary)
      ├─ enrollment (finger + TOTP secret)
      ├─ clock sync
      └─ bond management
```

---

## Workspace Crates

| Crate | Target | Description |
|-------|--------|-------------|
| `cyberkey-core` | `no_std` | TOTP generation (RFC 6238), BCD helpers |
| `fingerprint2-rs` | `no_std` | UART driver for the M5Stack Fingerprint2 sensor |
| `cyberkey-hid` | `no_std` | ASCII → HID keycode lookup table |
| `cyberkey-cli` | `std` binary | Desktop configuration tool |
| `firmware` | ESP32 (Xtensa) | Hardware init, event loop, all I/O |

The `no_std` split is intentional: all logic that can be tested on a laptop should be in a portable crate. See [docs/testing.md](docs/testing.md).

---

## Key Design Decisions (quick reference)

| Decision | Short answer | Full rationale |
|----------|-------------|----------------|
| NimBLE, not Bluedroid | NVS bond persistence, lower RAM | [docs/ble-security.md](docs/ble-security.md) |
| LESC + MITM passkey | Passive eavesdropping & rogue-pairing protection | [docs/ble-security.md](docs/ble-security.md) |
| NVS encrypted with eFuse key | Secrets survive extraction only with chip | [docs/storage.md](docs/storage.md) |
| `heapless::Vec`, no allocator | No fragmentation over weeks of runtime | [docs/totp.md](docs/totp.md) |
| Button polling, not interrupts | Simpler debounce and long-press logic | [docs/hardware.md](docs/hardware.md) |
| Power-off via GPIO4, not sleep | Sleep modes attempted but never worked reliably on this hw | [docs/hardware.md](docs/hardware.md) |
| JSON over serial | Human-readable, debuggable with any terminal | [docs/cli-protocol.md](docs/cli-protocol.md) |
| Numpad keycodes for digits | Layout-independent (works on AZERTY/QWERTY) | [docs/totp.md](docs/totp.md) |
| Custom proportional Orbitron font | Cyberpunk aesthetic; proportional spacing vs. `embedded-graphics` fixed grid | [docs/custom-font.md](docs/custom-font.md) |

---

## Documentation Pages

- **[docs/hardware.md](docs/hardware.md)** — GPIO map, peripherals, power budget, button polling, power-off strategy
- **[docs/ble-security.md](docs/ble-security.md)** — NimBLE, LESC/MITM, pairing flow, bond persistence
- **[docs/storage.md](docs/storage.md)** — NVS encryption, eFuse key, config layout, RTC persistence
- **[docs/totp.md](docs/totp.md)** — RFC 6238 implementation, `no_std` choices, clock sync, HID keycode mapping
- **[docs/cli-protocol.md](docs/cli-protocol.md)** — JSON wire protocol, command reference, enrollment flow, session auth
- **[docs/testing.md](docs/testing.md)** — Test strategy, portable vs. hardware tests
- **[docs/custom-font.md](docs/custom-font.md)** — How to convert a TTF font to Rust bitmap tables for the ST7789V2 display

---

## Source Map

```
firmware/src/
├── main.rs          hardware init (UART, SPI, I2C, GPIO, NVS, BLE)
├── app.rs           main event loop (buttons, fingerprint, BLE, display)
├── ble_hid.rs       NimBLE init, pairing, HID keystroke transmission
├── cli.rs           JSON command handler (FreeRTOS task)
├── fingerprint.rs   Fingerprint2Driver wrapper
├── display.rs       ST7789V2 drawing helpers
├── config_store.rs  NVS read/write
├── rtc.rs           BM8563 time sync
├── buttons.rs       polling-based button events
├── board.rs         board-level constants (GPIO map, SPI MHz, ADC calibration)
└── fonts/           custom Orbitron bitmap fonts (mini/regular/large)

crates/
├── cyberkey-core/   TOTP engine + BCD helpers
├── fingerprint2-rs/ sensor UART driver
├── cyberkey-hid/    ASCII → HID table
└── cyberkey-cli/    desktop CLI binary
```
