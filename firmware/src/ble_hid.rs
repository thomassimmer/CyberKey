//! BLE HID keyboard — NimBLE stack via esp32-nimble.
//!
//! Exposes:
//!   - `CONNECTED`   : AtomicBool  (true when CCCD subscribed, keystrokes can be sent)
//!   - `CLEAR_BONDS` : AtomicBool  (set by UI to request bond wipe + reboot)
//!   - `init(passkey)` → `BleHid`  (call once from main before the main loop)
//!   - `BleHid::type_string(text)`  (send keystrokes)
//!   - `clear_bonds_and_reboot()`   (wipe NVS bonds, then esp_restart)

use std::sync::atomic::{AtomicBool, Ordering};

use esp32_nimble::{
    enums::{AuthReq, OwnAddrType, PairKeyDist, SecurityIOCap},
    utilities::BleUuid,
    BLEAdvertisementData, BLECharacteristic, BLEDevice, BLEHIDDevice, NimbleSub,
};
use esp_idf_svc::hal::delay::FreeRtos;

use crate::hid::ascii_to_key;

// ---------------------------------------------------------------------------
// HID Report Descriptor (minimal boot keyboard — 45 bytes)
// ---------------------------------------------------------------------------

const HID_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    // Modifier byte
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7,
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    // Reserved byte
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
    // Key array (6 simultaneous keys)
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65,
    0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,
    0xC0, // End Collection
];

// ---------------------------------------------------------------------------
// Shared state (BLE task ↔ UI loop)
// ---------------------------------------------------------------------------

/// True when the host has written [0x01, 0x00] to the CCCD (notifications enabled).
/// Only set this state when it is safe to call notify() on the input_report.
static SUBSCRIBED: AtomicBool = AtomicBool::new(false);

/// Exposed to the UI loop: same meaning as SUBSCRIBED (safe to type keystrokes).
pub static CONNECTED: AtomicBool = AtomicBool::new(false);

/// UI sets this to request a bond-clear + reboot. Checked each iteration of
/// the main loop and also honoured here after disconnect.
pub static CLEAR_BONDS: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// BleHid — opaque handle returned by init()
// ---------------------------------------------------------------------------

type InputHandle = std::sync::Arc<
    esp32_nimble::utilities::mutex::Mutex<BLECharacteristic>,
>;

pub struct BleHid {
    input: InputHandle,
}

impl BleHid {
    /// Type every printable ASCII character in `text` as key-down / key-up pairs.
    ///
    /// No-op if the host has not yet subscribed to HID notifications.
    pub fn type_string(&self, text: &str) {
        if !SUBSCRIBED.load(Ordering::Relaxed) {
            log::warn!("[HID] not subscribed — dropping '{}'", text);
            return;
        }
        for byte in text.bytes() {
            let (modifier, keycode) = ascii_to_key(byte);
            if keycode == 0 {
                log::debug!("[HID] skipping unmapped char 0x{:02x}", byte);
                continue;
            }
            self.input
                .lock()
                .set_value(&[modifier, 0x00, keycode, 0, 0, 0, 0, 0])
                .notify();
            FreeRtos::delay_ms(10); // hold key down
            self.input.lock().set_value(&[0u8; 8]).notify(); // key up
            FreeRtos::delay_ms(5);
        }
        log::info!("[HID] '{}' sent", text);
    }
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

/// Initialise the NimBLE HID stack.  Call once from `main()` before the loop.
///
/// `passkey` — 6-digit PIN shown on the LCD; the host must enter it during
/// first pairing.  After bonding, macOS reconnects without user input.
pub fn init(passkey: u32) -> BleHid {
    let device = BLEDevice::take();

    // Log any bonds already stored in NVS — useful for diagnosing reconnection.
    match device.bonded_addresses() {
        Ok(addrs) if addrs.is_empty() => log::info!("[BLE] NVS bonds: none"),
        Ok(addrs) => {
            log::info!("[BLE] NVS bonds: {} stored", addrs.len());
            for (i, addr) in addrs.iter().enumerate() {
                log::info!("[BLE]   bond[{}]: {:?}", i, addr);
            }
        }
        Err(e) => log::warn!("[BLE] failed to read bonds: {:?}", e),
    }

    BLEDevice::set_device_name("CyberKey").unwrap();

    // RPA: the advertised address rotates periodically; macOS resolves it via the
    // IRK distributed during bonding.  This enables automatic reconnection after
    // a power cycle without user interaction.
    device.set_own_addr_type(OwnAddrType::RpaPublicDefault);

    device
        .security()
        .set_auth(AuthReq::Bond | AuthReq::Mitm)
        .set_io_cap(SecurityIOCap::DisplayOnly)
        .set_passkey(passkey)
        .set_security_init_key(PairKeyDist::ENC | PairKeyDist::ID)
        .set_security_resp_key(PairKeyDist::ENC | PairKeyDist::ID);

    let server = device.get_server();

    // NOTE: do NOT reset SUBSCRIBED in on_connect.  On reconnection with a stored
    // bond, macOS writes the CCCD (re-enabling notifications) *before* the GAP
    // connect event fires — resetting here would erase that subscription.
    server.on_connect(|_, desc| {
        log::info!("[BLE] link connected: {:?}", desc.address());
    });
    server.on_disconnect(|desc, _| {
        log::info!("[BLE] disconnected: {:?}", desc.address());
        SUBSCRIBED.store(false, Ordering::Relaxed);
        CONNECTED.store(false, Ordering::Relaxed);
    });

    let mut hid = BLEHIDDevice::new(server);
    let input = hid.input_report(0);

    input.lock().on_subscribe(|_chr, _desc, sub| {
        let notifying = sub.contains(NimbleSub::NOTIFY);
        log::info!("[BLE] CCCD write — notify={}", notifying);
        SUBSCRIBED.store(notifying, Ordering::Relaxed);
        CONNECTED.store(notifying, Ordering::Relaxed);
    });

    hid.manufacturer("CyberKey");
    // PnP ID: vendor source = USB-IF (0x02), vendor = Apple (0x05AC).
    // Using Apple's vendor ID causes macOS to apply its optimised HID driver.
    hid.pnp(0x02, 0x05ac, 0x820a, 0x0210);
    hid.hid_info(0x00, 0x01);
    hid.report_map(HID_DESCRIPTOR);
    hid.set_battery_level(100);

    device
        .get_advertising()
        .lock()
        .set_data(
            BLEAdvertisementData::new()
                .name("CyberKey")
                .appearance(0x03C1) // Bluetooth SIG: Keyboard
                .add_service_uuid(BleUuid::Uuid16(0x1812)),
        )
        .unwrap();
    device.get_advertising().lock().start().unwrap();

    log::info!("[BLE] advertising — PIN for first pairing: {:06}", passkey);

    BleHid { input }
}

// ---------------------------------------------------------------------------
// Bond clearing
// ---------------------------------------------------------------------------

/// Delete all NimBLE bond data and reboot the device.
///
/// After reboot the device will advertise openly and require re-pairing.
pub fn clear_bonds_and_reboot() -> ! {
    log::info!("[BLE] clearing all bonds — rebooting");
    let device = BLEDevice::take();
    let _ = device.delete_all_bonds();
    unsafe { esp_idf_svc::sys::esp_restart() }
}
