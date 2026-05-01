# BLE Security

## Stack Choice: NimBLE, not Bluedroid

ESP-IDF ships with two BLE stacks:

- **Bluedroid**: ported from Android. Feature-complete but heavy (~200 KB RAM).
- **NimBLE**: Apache NimBLE, purpose-built for embedded. Lighter, actively maintained for ESP-IDF, and — critically — it persists BLE bonds in NVS out of the box via `CONFIG_BT_NIMBLE_NVS_PERSIST=y`.

The bond persistence was the deciding factor. Without it, the host OS would require manual re-pairing every time the device reboots. macOS caches the LTK (Long-Term Key) and uses it to reconnect silently on next connection — but only if the device still has the matching LTK on its side. NimBLE + NVS makes that work automatically.

---

## Pairing Security: LESC + MITM

BLE has several security modes. Without any protection, a nearby device can silently pair and send keystrokes. That would be a serious problem for a device whose only purpose is typing authentication codes.

CyberKey uses:

- **LESC** (LE Secure Connections): key exchange via ECDH P-256. Neither side sends the link key in the clear; it is derived from the exchange. Passive eavesdropping on the pairing exchange cannot recover the LTK.
- **MITM** (Man-in-the-Middle protection): requires user confirmation to prevent an attacker from substituting their own public key during pairing. The confirmation method is **passkey entry** (see below).

### I/O Capability: DisplayOnly

BLE MITM protection relies on matching the I/O capabilities of both devices to pick the right "association model":

| Device capability  | Host capability    | Association model    |
| ------------------ | ------------------ | -------------------- |
| Display only       | Keyboard           | Passkey entry        |
| No I/O             | Any                | Just Works (no MITM) |
| Keyboard + Display | Keyboard + Display | Numeric comparison   |

CyberKey declares `DisplayOnly`: it can show a number but cannot accept input. The host OS has a keyboard. This maps to the **passkey entry** model:

1. CyberKey generates a random 6-digit passkey at boot using the ESP32 hardware RNG (`esp_random()`).
2. The passkey is displayed on the LCD.
3. The host prompts the user to type those 6 digits.
4. Both sides derive the LTK using the passkey as an authenticator.

A passive attacker who intercepts the pairing traffic cannot compute the LTK without knowing the passkey. A rogue device cannot forge the exchange because it cannot display the real passkey.

---

## Pairing Flow

```
Boot (no bonds in NVS)
│
├─ Generate random passkey (e.g., 482 916)
├─ Display on LCD: ">> BT PAIRING << / 482 916"
└─ pairing_open = true (advertising with MITM flag)

Host initiates pairing
│
├─ ECDH P-256 key exchange
├─ Host prompts: "Enter code from device"
└─ User types 482916

Pairing completes
│
├─ LTK stored in NVS (both sides)
├─ pairing_open = false
└─ Display: "Connected"
```

Subsequent connections are automatic: the host sends its identity resolving key (IRK), NimBLE matches it to a stored bond, and the encrypted session resumes without user interaction.

### Opening and Closing the Pairing Window

- **Manual open/close**: Button B short-press toggles the pairing window. When open, a 6-digit random passkey is displayed.
- **Silent Background Sync**: at boot, if bonds exist, the device advertises for 15 seconds without allowing new pairings (`PAIRING_ALLOWED = false`). This allows known hosts to reconnect silently in the background.
- **Auto-open at boot**: if NVS contains no bonds, the pairing window opens automatically.
- **Auto-close**: window closes after 60 seconds with no pairing, or manually via Button B.
- **Multi-host support**: up to 3 simultaneous connections are supported. HID reports are broadcast to all connected and subscribed hosts.
- **Clear bonds**: Button A long-press × 2 erases all NVS bonds and reboots.

---

## Bond Persistence

NimBLE with `CONFIG_BT_NIMBLE_NVS_PERSIST=y` writes the LTK and IRK for each bonded peer into the `"nimble"` NVS namespace automatically after pairing. On next boot, NimBLE loads them back and can resume encrypted sessions without re-pairing.

The NVS partition containing bonds is the same encrypted partition used for TOTP secrets (AES-256-XTS). See [docs/storage.md](storage.md) for the encryption details.

---

## HID Keyboard Profile

The device advertises as a BLE HID keyboard using the standard GATT HID service (UUID 0x1812). The HID report descriptor declares an 8-byte boot keyboard report:

```
Byte 0:  Modifier (Ctrl, Shift, Alt, GUI — left and right, 1 bit each)
Byte 1:  Reserved (always 0x00)
Bytes 2–7: Keycodes (up to 6 simultaneous keys)
```

A keystroke is sent as two reports back-to-back: key-down (10 ms hold), then key-up (all zeros). The firmware adds a 5 ms inter-key gap to avoid hosts dropping rapid keystrokes.

**Layout independence**: TOTP digits are sent using numpad keycodes (USB HID page 0x07, usage 0x59–0x62) instead of number-row keycodes (0x1E–0x27). Number-row keycodes map to physical positions, which differ between QWERTY and AZERTY. Numpad keycodes are always interpreted as digits 0–9 by the OS, regardless of the active keyboard layout.

---

## Threat Model (BLE)

**Rogue pairing**: attacker tries to pair with CyberKey before the legitimate host does.
Mitigation: MITM passkey entry. Without seeing the display, the attacker cannot complete pairing.
Residual risk: if the user confirms a pairing without checking the passkey on the LCD, a rogue device could pair. User must verify the PIN.

**Passive eavesdropping**: attacker records the BLE pairing exchange and tries to derive the LTK.
Mitigation: LESC uses ECDH — the key material never crosses the air. Captured packets cannot be used to derive the LTK without knowing the passkey.

**Replay attacks on HID traffic**: attacker captures and replays HID reports from a previous session.
Mitigation: the BLE session encryption key changes per session. Replayed reports from a previous session are rejected at the BLE layer.

**HID hijacking on a compromised host**: malware on the host intercepts or alters HID input.
Not mitigated: this is a fundamental limitation of the HID protocol. The device cannot control what the host OS does with its reports.
