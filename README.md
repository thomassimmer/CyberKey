# cyberkey

BLE HID keyboard for the M5StickC Plus 2 (ESP32), with TOTP and fingerprint authentication.

## Workspace layout

| Crate | Target | Description |
|---|---|---|
| `crates/cyberkey-core` | host + ESP32 | TOTP engine, config schema, BCD utils |
| `crates/cyberkey-hid` | host + ESP32 | HID keycode table |
| `crates/fingerprint2-rs` | host + ESP32 | Fingerprint sensor driver |
| `crates/cyberkey-cli` | host | Desktop CLI for device management |
| `firmware` | ESP32 only | Firmware binary (ESP-IDF / NimBLE) |

## Testing

The host-portable crates can be tested with the standard Rust toolchain:

```sh
cargo test --package cyberkey-core
cargo test --package cyberkey-hid
cargo test --package fingerprint2-rs
cargo test --package cyberkey-cli
```

`firmware` targets the Xtensa ESP32 and depends on ESP-IDF; it requires the
Espressif toolchain (`espup`) and cannot be compiled for the host.
`cargo test --workspace` is intentionally not used from the root for this reason.

## Firmware build

```sh
cd firmware
cargo build --release
cargo run --release   # flash via espflash
```