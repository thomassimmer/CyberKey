//! BLE HID keyboard — NimBLE stack via esp32-nimble.
//!
//! Exposes:
//!   - `CONNECTED`   : AtomicBool  (true when CCCD subscribed, keystrokes can be sent)
//!   - `CLEAR_BONDS` : AtomicBool  (set by UI to request bond wipe + reboot)
//!   - `init(passkey)` → `BleHid`  (call once from main before the main loop)
//!   - `BleHid::type_digits(text)`  (send numpad digit keystrokes)
//!   - `clear_bonds_and_reboot()`   (wipe NVS bonds, then esp_restart)

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use esp32_nimble::{
    enums::{AuthReq, OwnAddrType, PairKeyDist, SecurityIOCap},
    utilities::BleUuid,
    BLEAdvertisementData, BLECharacteristic, BLEDevice, BLEHIDDevice, NimbleSub,
};
use esp_idf_svc::hal::delay::FreeRtos;

// ---------------------------------------------------------------------------
// HID Report Descriptor (minimal boot keyboard — 45 bytes)
// ---------------------------------------------------------------------------

const HID_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    // Modifier byte
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    // Reserved byte
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01, // Key array (6 simultaneous keys)
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,
    0xC0, // End Collection
];

// ---------------------------------------------------------------------------
// Shared state (BLE task ↔ UI loop)
// ---------------------------------------------------------------------------

/// True when the host has written [0x01, 0x00] to the CCCD (notifications enabled).
/// Only set this state when it is safe to call notify() on the input_report.
static SUBSCRIBED: AtomicU32 = AtomicU32::new(0);

/// Exposed to the UI loop: number of active BLE links.
pub static CONNECTED: AtomicU32 = AtomicU32::new(0);

/// UI sets this to request a bond-clear + reboot. Checked each iteration of
/// the main loop and also honoured here after disconnect.
pub static CLEAR_BONDS: AtomicBool = AtomicBool::new(false);

/// Passkey shown on screen when the pairing window is open.
static CURRENT_PASSKEY: AtomicU32 = AtomicU32::new(0);

/// True only while the explicit pairing window is active.  When false the
/// on_passkey_request callback returns random junk so new pairings fail.
pub static PAIRING_ALLOWED: AtomicBool = AtomicBool::new(false);

/// CLI sets this flag; the main loop picks it up and opens a fresh window.
pub static OPEN_PAIRING_REQUESTED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// BleHid — opaque handle returned by init()
// ---------------------------------------------------------------------------

type InputHandle = std::sync::Arc<esp32_nimble::utilities::mutex::Mutex<BLECharacteristic>>;

pub struct BleHid {
    input: InputHandle,
}

impl BleHid {
    /// Type a string of digits using numpad keycodes (layout-independent).
    ///
    /// Regular digit keycodes (0x1e–0x27) are physical-position-based and produce
    /// wrong characters on non-QWERTY layouts (e.g. AZERTY gives "àéèç…").
    /// Numpad keycodes (0x59–0x62) are always interpreted as digits by the host OS.
    pub fn type_digits(&self, digits: &str) {
        if SUBSCRIBED.load(Ordering::Relaxed) == 0 {
            log::warn!("[HID] no active subscriptions — dropping digits");
            return;
        }
        for byte in digits.bytes() {
            let keycode = match byte {
                b'1' => 0x59u8,
                b'2' => 0x5A,
                b'3' => 0x5B,
                b'4' => 0x5C,
                b'5' => 0x5D,
                b'6' => 0x5E,
                b'7' => 0x5F,
                b'8' => 0x60,
                b'9' => 0x61,
                b'0' => 0x62,
                _ => continue,
            };
            self.input
                .lock()
                .set_value(&[0x00, 0x00, keycode, 0, 0, 0, 0, 0])
                .notify();
            FreeRtos::delay_ms(10);
            self.input.lock().set_value(&[0u8; 8]).notify();
            FreeRtos::delay_ms(5);
        }
        log::info!("[HID] digits sent");
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

    BLEDevice::set_device_name("CyberKey").expect("BLE: failed to set device name");

    // RPA: the advertised address rotates periodically; macOS resolves it via the
    // IRK distributed during bonding.  This enables automatic reconnection after
    // a power cycle without user interaction.
    device.set_own_addr_type(OwnAddrType::RpaPublicDefault);

    CURRENT_PASSKEY.store(passkey, Ordering::Relaxed);
    device
        .security()
        .set_auth(AuthReq::Bond | AuthReq::Mitm)
        .set_io_cap(SecurityIOCap::DisplayOnly)
        .set_security_init_key(PairKeyDist::ENC | PairKeyDist::ID)
        .set_security_resp_key(PairKeyDist::ENC | PairKeyDist::ID);

    let server = device.get_server();

    // NOTE: do NOT reset SUBSCRIBED in on_connect.  On reconnection with a stored
    // bond, macOS writes the CCCD (re-enabling notifications) *before* the GAP
    // connect event fires — resetting here would erase that subscription.
    server.on_connect(|_, desc| {
        let count = CONNECTED.fetch_add(1, Ordering::Relaxed) + 1;
        log::info!(
            "[BLE] link connected: {:?} (total: {})",
            desc.address(),
            count
        );
        // If the pairing window is still open and we have slots, keep/restart advertising.
        if PAIRING_ALLOWED.load(Ordering::Relaxed) && count < 3 {
            let _ = BLEDevice::take().get_advertising().lock().start();
        }
    });
    server.on_disconnect(|desc, _| {
        let count = CONNECTED
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(1)))
            .unwrap_or(0)
            .saturating_sub(1);
        log::info!(
            "[BLE] disconnected: {:?} (total: {})",
            desc.address(),
            count
        );
        // We don't automatically restart advertising here; advertising is only
        // enabled while the pairing window is explicitly open.
        if count == 0 {
            SUBSCRIBED.store(0, Ordering::Relaxed);
        }
    });
    // Return the real passkey only when the pairing window is explicitly open.
    // Bonded peers reconnect via LTK and never trigger this callback.
    server.on_passkey_request(|| {
        if PAIRING_ALLOWED.load(Ordering::Relaxed) {
            CURRENT_PASSKEY.load(Ordering::Relaxed)
        } else {
            (unsafe { esp_idf_svc::sys::esp_random() }) % 1_000_000
        }
    });

    let mut hid = BLEHIDDevice::new(server);
    let input = hid.input_report(0);

    input.lock().on_subscribe(|_chr, _desc, sub| {
        let notifying = sub.contains(NimbleSub::NOTIFY);
        log::info!("[BLE] CCCD write — notify={}", notifying);
        if notifying {
            SUBSCRIBED.fetch_add(1, Ordering::Relaxed);
        } else {
            SUBSCRIBED
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(1)))
                .ok();
        }
    });

    hid.manufacturer("CyberKey");
    // PnP ID: vendor source = USB-IF (0x02), vendor = Apple (0x05AC).
    // Using Apple's vendor ID causes macOS to apply its optimised HID driver.
    hid.pnp(0x02, 0x05ac, 0x820a, 0x0210);
    hid.hid_info(0x00, 0x01);
    hid.report_map(HID_DESCRIPTOR);
    hid.set_battery_level(100);

    // Advertising is NOT started here; it's only enabled when the pairing
    // window is explicitly opened by the user.

    log::info!("[BLE] advertising — PIN for first pairing: {:06}", passkey);

    BleHid { input }
}

// ---------------------------------------------------------------------------
// Bond clearing
// ---------------------------------------------------------------------------

/// Delete all NimBLE bond data without rebooting.
pub fn clear_bonds() {
    log::info!("[BLE] clearing all bonds");
    let device = BLEDevice::take();
    let _ = device.delete_all_bonds();
}

/// Delete all NimBLE bond data and reboot the device.
///
/// After reboot the device will advertise openly and require re-pairing.
pub fn clear_bonds_and_reboot() -> ! {
    clear_bonds();
    unsafe { esp_idf_svc::sys::esp_restart() }
}

// ---------------------------------------------------------------------------
// Pairing window
// ---------------------------------------------------------------------------

/// True if at least one bond is stored in NVS.
pub fn has_bonds() -> bool {
    BLEDevice::take()
        .bonded_addresses()
        .map(|a| !a.is_empty())
        .unwrap_or(false)
}

/// Open the pairing window: arm CURRENT_PASSKEY and set PAIRING_ALLOWED.
///
/// The on_passkey_request callback will now return `passkey` so a host that
/// types the displayed PIN can complete pairing.
pub fn open_pairing_window(passkey: u32) {
    CURRENT_PASSKEY.store(passkey, Ordering::Relaxed);
    PAIRING_ALLOWED.store(true, Ordering::Relaxed);
    log::info!("[BLE] pairing window open — PIN {:06}", passkey);
    start_advertising();
}

/// Start advertising for reconnection only (no new pairings allowed).
pub fn start_background_sync() {
    PAIRING_ALLOWED.store(false, Ordering::Relaxed);
    log::info!("[BLE] background sync started (bonded only)");
    start_advertising();
}

fn start_advertising() {
    let device = BLEDevice::take();
    let mut adv = device.get_advertising().lock();
    if let Err(e) = adv.set_data(
        BLEAdvertisementData::new()
            .name("CyberKey")
            .appearance(0x03C1)
            .add_service_uuid(BleUuid::Uuid16(0x1812)),
    ) {
        log::error!("[BLE] set_data failed: {:?}", e);
    }
    if let Err(e) = adv.start() {
        log::error!("[BLE] adv.start() failed: {:?}", e);
    }
}

/// Close the pairing window: stop advertising and clear PAIRING_ALLOWED.
pub fn close_pairing_window() {
    PAIRING_ALLOWED.store(false, Ordering::Relaxed);
    let _ = BLEDevice::take().get_advertising().lock().stop();
    log::info!("[BLE] pairing window closed");
}
