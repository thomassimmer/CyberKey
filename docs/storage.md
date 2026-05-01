# Storage

## NVS Overview

ESP-IDF's NVS (Non-Volatile Storage) is a key-value store backed by flash. It organizes data into **namespaces** (like directories) with string keys and typed values (u8, i32, str, blob, etc.).

CyberKey uses the `"ck"` namespace for all application data. The NVS partition is separate from the firmware partition — a factory reset or re-flash of the firmware does not wipe NVS unless you explicitly erase it.

---

## Encryption: Why eFuse-backed AES-256-XTS

TOTP secrets stored in flash are readable by anyone with a USB-JTAG adapter and the right tool if the flash is unencrypted. On an ESP32, the full flash image can be dumped in seconds.

ESP-IDF's secure NVS uses **AES-256-XTS** (tweakable block cipher, standard for storage encryption). The key is:

1. Generated randomly (from hardware RNG) on the very first boot.
2. Burned into the ESP32's **eFuses** (one-time programmable fuses inside the chip).
3. The eFuse key read protection is enabled, so the key cannot be read back by software afterward.

After the first boot, the key exists only inside the silicon. An attacker who desolds the flash chip gets ciphertext with no key. An attacker who runs code on the ESP32 can ask the hardware to *use* the key (for NVS decrypt/encrypt) but cannot extract it.

**How it is initialized in firmware (`config_store.rs`):**

```rust
let nvs = match EspNvsPartition::<NvsEncrypted>::take() {
    Ok(p) => p,
    Err(_) => {
        // First boot, or eFuse key generation failed:
        // erase the partition so we start clean
        nvs_flash_erase("nvs");
        EspNvsPartition::<NvsEncrypted>::take().expect("NVS init failed")
    }
};
```

The fallback branch (erase + reinit) handles the case where the partition was previously unencrypted or got corrupted. It means data loss, but leaves the device in a known-good state rather than an unbootable one. This is the correct tradeoff for a security device.

---

## NVS Layout

All keys are in the `"ck"` namespace:

| Key | Type | Example value | Description | Cleared on reset |
|-----|------|---------------|-------------|-----------------|
| `slot_0` | str | `JBSWY3DPEHPK3PXP` | TOTP base32 secret for slot 0 | yes |
| `label_0` | str | `GitHub` | Human-readable label for slot 0 | yes |
| `slot_1` | str | `AAAAAAAAAAAA` | Secret for slot 1 | yes |
| `label_1` | str | `AWS` | Label for slot 1 | yes |
| ... | | | Slots 0–9 (max 10 entries) | yes |
| `tz_offset` | i32 | `7200` | UTC offset in seconds (e.g., 7200 = UTC+2) | yes |

NimBLE also stores bond data (LTK, IRK, CCCD) in its own NVS namespace. That namespace is separate from `"ck"` but is also cleared on factory reset via `BLEDevice::delete_all_bonds()`.

There is no index or count stored — the firmware scans for `slot_0` through `slot_9` on startup and builds the in-memory `CyberKeyConfig` from what exists.

**Why store base32 secrets in plain text inside NVS?**

They *are* encrypted at the flash level by AES-256-XTS. "Plain text in NVS" means "not additionally hashed or encoded inside the key-value store" — the NVS itself is the encryption layer. Hashing the secrets before storage would make it impossible to retrieve them for TOTP generation, so that would not work.

---

## Config Store Threading Model

`ConfigStore` is wrapped in `Arc<Mutex<ConfigStore>>` and shared between the main loop and the CLI FreeRTOS task.

The Mutex is an `esp_idf_svc::sync::Mutex` (backed by a FreeRTOS mutex), not `std::sync::Mutex`. It supports cross-task locking.

**Invariant**: NVS writes always happen atomically from the perspective of the stored config:

- When adding an entry, the fingerprint template is enrolled first. If NVS write fails afterward, the firmware calls `fp.delete_template(slot)` before returning the error to the CLI. The caller never sees a state where the sensor has a template but NVS does not.
- When removing an entry, NVS is erased first, then the fingerprint template. If the device loses power between these two steps, the slot is orphaned in the sensor but the NVS entry is gone — the device behaves as if the entry does not exist (the slot is just unreachable). Recovery: factory reset.

---

## RTC and Timezone Persistence

Two additional pieces of state are persisted across reboots:

### Unix Timestamp (BM8563 RTC)

The BM8563 RTC retains time as long as it has power (main battery or backup capacitor). The firmware reads it at boot via I2C. If the VL bit is set (power was lost), the firmware falls back to the `BUILD_TIME` constant embedded at compile time.

TOTP generation uses the system clock (`SystemTime::now()`), which is set from the RTC at boot via `settimeofday`. If the RTC is wrong, TOTP codes are wrong. This is why `sync_clock` is the first command the CLI sends in every session.

### UTC Offset (NVS `tz_offset`)

The UTC offset is stored separately in NVS (not in the RTC, which only stores UTC). At boot:

- TOTP generation uses the raw UTC timestamp — no offset applied.
- The display shows local time by adding `tz_offset` to the UTC timestamp for display only.

This means the device shows the correct local time even without a CLI session, as long as the timezone was synced at least once.

---

## Factory Reset

A factory reset wipes all user data. It can be triggered two ways:

- **Physical button**: hold Button A for 2 s at boot, then press it again within 10 s to confirm.
- **CLI**: `{"cmd":"factory_reset","confirm":"RESET"}` (requires an authenticated session).

Both paths call the same `do_factory_reset()` function in `app.rs`, which clears in order:

1. Fingerprint templates — `fp.empty_template_library()` (sensor internal flash)
2. NVS TOTP data — `slot_0`–`slot_9` and `label_0`–`label_9`
3. NVS timezone — `tz_offset`
4. BLE bonds — `BLEDevice::delete_all_bonds()` (NimBLE NVS namespace)
5. `esp_restart()`

**What survives a factory reset:**

| Data | Where | Notes |
|------|-------|-------|
| BM8563 RTC timestamp | Hardware registers | Persists until power loss; run `sync_clock` after reset |
| Firmware binary | Flash (OTA partition) | Intentional — reset is data-only |
| NVS encryption key | eFuses | Burned once at first boot, cannot be erased in software |
