# Repository Guidelines

CyberKey is a Rust workspace for a Bluetooth HID key device built on the M5StickC Plus 2 (ESP32). It stores TOTP secrets per enrolled finger and types OTPs via BLE. See [ARCHITECTURE.md](ARCHITECTURE.md) for a full system diagram.

---

## Project Structure

```
Cargo.toml              # Workspace root (resolver = "2")
firmware/               # ESP32 firmware (Xtensa target, no_std + esp-idf)
  src/                  # main.rs, app.rs, ble_hid.rs, cli.rs, …
crates/
  cyberkey-core/        # TOTP engine + BCD helpers (no_std, portable)
  fingerprint2-rs/      # UART driver for the fingerprint sensor (no_std, portable)
  cyberkey-hid/         # ASCII → HID keycode table (no_std, portable)
  cyberkey-cli/         # Desktop configuration binary (std)
docs/                   # Design documents (BLE, storage, testing, protocol, …)
tools/                  # Helper scripts (cli_test.py)
```

The `no_std` split is intentional: all logic that does not touch hardware belongs in a portable crate that can be unit-tested on a laptop.

---

## Build, Test & Development Commands

```bash
# Run all portable unit tests (excludes firmware)
cargo test --exclude firmware

# Run tests for a single crate
cargo test --package cyberkey-core

# Check formatting
cargo fmt --all -- --check

# Apply formatting
cargo fmt --all

# Lint (warnings are errors in CI)
cargo clippy --package cyberkey-core -- -D warnings

# Build & flash firmware (requires Xtensa toolchain — see below)
cd firmware && cargo run --release
```

**Firmware toolchain setup (one-time):**
```bash
cargo install espup && espup install
source ~/.espup/export-esp.sh
```

The workspace root uses the `stable` toolchain; `firmware/` uses the `esp` channel declared in `firmware/rust-toolchain.toml`.

---

## Coding Style & Naming Conventions

- **Formatter:** `rustfmt` with default settings — run `cargo fmt --all` before committing.
- **Linter:** `cargo clippy` with `-D warnings`; all warnings must be resolved.
- **Naming:** follow standard Rust conventions — `snake_case` for functions/variables, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- **Error handling:** use `Result`/`Option` and propagate errors with `?`. Avoid `unwrap` in library code; it is acceptable in `main` or test helpers only.
- **`no_std` crates:** do not introduce `std`-only dependencies. Prefer `heapless` collections over `Vec`/`HashMap`.

---

## Testing Guidelines

Unit tests live in the same file as the code they test (`#[cfg(test)]` modules). The firmware crate has no unit tests — all testable logic must live in a portable crate.

```bash
cargo test --package cyberkey-core      # TOTP, config validation, serialization
cargo test --package fingerprint2-rs    # packet framing, checksum, mock UART
cargo test --package cyberkey-hid       # keycode table completeness
cargo test --package cyberkey-cli       # JSON serialization, response parsing
```

Name test functions descriptively: `test_<unit>_<scenario>` (e.g., `test_totp_rfc6238_vector_t0`). Hardware smoke tests are manual — see [docs/testing.md](docs/testing.md) for the full checklist.

---

## Commit & Pull Request Guidelines

Follow the **Conventional Commits** style observed in the project history:

```
<type>(<scope>): <short description>

# Examples
feat(firmware): add factory reset hold check
fix: resolve fingerprint sensor unresponsiveness during CLI auth
chore: fmt
perf(fingerprint): implement low-power sleep mode
```

Common types: `feat`, `fix`, `chore`, `perf`, `ci`, `doc`, `refactor`.

**Pull requests should:**
- Target `main`.
- Pass all CI jobs (`test`, `clippy`, `fmt`, `doc`) before review.
- Include a short description of *what* changed and *why*.
- Reference any related issue with `Closes #<n>` when applicable.

---

## CI

Four jobs run on every push and pull request (portable crates only — firmware is excluded):

| Job | Command |
|-----|---------|
| `test` | `cargo test --package <each portable crate>` |
| `clippy` | `cargo clippy --package <each portable crate> -- -D warnings` |
| `fmt` | `cargo fmt --all -- --check` |
| `doc` | `cargo doc --no-deps --package <each portable crate>` |

All jobs must be green before merging.
