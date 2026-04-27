//! CLI wire protocol — listens on UART0 (USB-serial) for JSON newline-delimited commands.
//!
//! Each command is a JSON object with a `"cmd"` field, e.g.:
//!   {"cmd":"ping"}
//!   {"cmd":"list_entries"}
//!   {"cmd":"add_entry","label":"GitHub","secret_b32":"JBSWY3DPEHPK3PXP"}
//!   {"cmd":"remove_entry","label":"GitHub"}
//!   {"cmd":"sync_clock","timestamp":1745000000}
//!   {"cmd":"factory_reset","confirm":"RESET"}
//!
//! Each response is a JSON object on a single line followed by `\n`.
//! For `add_entry`, the firmware streams `{"event":"enroll_step",...}` messages
//! before the final `{"ok":true,"slot":N}` response.
//! Log output from EspLogger is interleaved on the same UART; the host tool
//! should filter for lines that start with `{`.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};

use esp_idf_svc::hal::{delay::BLOCK, uart::UartDriver};
use serde::{Deserialize, Serialize};

pub use crate::config_store::SharedNvs;

// ── Enrollment IPC (CLI task ↔ main loop) ────────────────────────────────────

/// Sent from the CLI task to the main loop to kick off a fingerprint enrollment.
pub struct EnrollRequest {
    /// Fingerprint sensor slot to enroll into.
    pub slot: u16,
    /// Channel for the main loop to stream progress events back to the CLI task.
    pub reply: mpsc::SyncSender<EnrollResp>,
}

/// Progress events sent from the main loop back to the CLI task during enrollment.
pub enum EnrollResp {
    /// Prompt the user to place their finger (step N of total).
    PlaceFinger { step: u8, total: u8 },
    /// One capture complete — prompt the user to lift their finger.
    LiftFinger { step: u8, total: u8 },
    /// All captures merged and stored successfully.
    Done,
    /// Enrollment failed (sensor error or begin_enroll rejected).
    Failed,
}

/// Set by [`cmd_factory_reset`] to signal the main loop to clear fingerprint templates and reboot.
pub static FACTORY_RESET: AtomicBool = AtomicBool::new(false);

/// Shared queue used to hand an [`EnrollRequest`] from the CLI task to the main loop.
///
/// `None` = no pending request; `Some(_)` = main loop should pick this up and
/// process it, then set back to `None`.
pub type EnrollQueue = Arc<Mutex<Option<EnrollRequest>>>;

// ── Wire types ────────────────────────────────────────────────────────────────

/// All fields that any command might carry.  Fields not relevant to a given
/// command are simply `None` after deserialisation.
#[derive(Deserialize)]
struct Cmd {
    cmd: String,
    /// Used by: `add_entry`, `remove_entry`
    label: Option<String>,
    /// Used by: `add_entry`
    secret_b32: Option<String>,
    /// Used by: `delete_entry` (legacy slot-based removal)
    slot: Option<u32>,
    /// Used by: `sync_clock` (legacy field name — keep for backward compat)
    ts: Option<u64>,
    /// Used by: `sync_clock` (protocol field name sent by cyberkey-cli)
    timestamp: Option<u64>,
    /// Used by: `factory_reset`
    confirm: Option<String>,
    /// Used by: `sync_clock` — seconds east of UTC (e.g. 7200 for UTC+2).
    tz_offset_secs: Option<i32>, // Option because Cmd is a flat catch-all struct
}

#[derive(Serialize)]
struct Resp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entries: Option<Vec<SlotEntry>>,
    /// Returned by `add_entry` on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<u8>,
}

#[derive(Serialize)]
struct SlotEntry {
    slot: u8,
    label: String,
    secret_masked: String,
}

/// Streaming enrollment event sent during `add_entry`.
#[derive(Serialize)]
struct EnrollEvent {
    event: &'static str,
    step: u8,
    total: u8,
    state: &'static str,
}

impl Resp {
    fn ok() -> Self {
        Resp { ok: true, error: None, entries: None, slot: None }
    }
    fn ok_slot(slot: u8) -> Self {
        Resp { ok: true, error: None, entries: None, slot: Some(slot) }
    }
    fn err(msg: impl Into<String>) -> Self {
        Resp { ok: false, error: Some(msg.into()), entries: None, slot: None }
    }
}

// ── Task entry ────────────────────────────────────────────────────────────────

/// Spawn the CLI listener as a dedicated FreeRTOS task.
pub fn spawn(
    uart: UartDriver<'static>,
    nvs: Arc<Mutex<SharedNvs>>,
    enroll_queue: EnrollQueue,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || run(uart, nvs, enroll_queue))?;
    Ok(())
}

fn run(uart: UartDriver<'static>, nvs: Arc<Mutex<SharedNvs>>, enroll_queue: EnrollQueue) {
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut byte = [0u8; 1];

    loop {
        if uart.read(&mut byte, BLOCK).is_err() {
            continue;
        }
        match byte[0] {
            b'\n' => {
                if !buf.is_empty() {
                    handle_command(&uart, &buf, &nvs, &enroll_queue);
                    buf.clear();
                }
            }
            b'\r' => {}
            b => {
                if buf.len() < 1024 {
                    buf.push(b);
                }
            }
        }
    }
}

// ── Write helpers ─────────────────────────────────────────────────────────────

fn write_resp(uart: &UartDriver<'static>, resp: &Resp) {
    if let Ok(mut out) = serde_json::to_vec(resp) {
        out.push(b'\n');
        let _ = uart.write(&out);
    }
}

fn write_event(uart: &UartDriver<'static>, event: &EnrollEvent) {
    if let Ok(mut out) = serde_json::to_vec(event) {
        out.push(b'\n');
        let _ = uart.write(&out);
    }
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

/// Top-level command router.  `add_entry` is handled specially because it
/// needs to stream multiple JSON lines before the terminal response; all other
/// commands produce a single `Resp` via `dispatch`.
fn handle_command(
    uart: &UartDriver<'static>,
    raw: &[u8],
    nvs: &Arc<Mutex<SharedNvs>>,
    enroll_queue: &EnrollQueue,
) {
    let s = match core::str::from_utf8(raw) {
        Ok(s) => s.trim(),
        Err(_) => return write_resp(uart, &Resp::err("invalid utf-8")),
    };
    if s.is_empty() {
        return;
    }
    let cmd = match serde_json::from_str::<Cmd>(s) {
        Ok(c) => c,
        Err(e) => return write_resp(uart, &Resp::err(format!("parse error: {e}"))),
    };

    if cmd.cmd == "add_entry" {
        cmd_add_entry(uart, &cmd, nvs, enroll_queue);
    } else {
        let resp = dispatch(&cmd, nvs);
        write_resp(uart, &resp);
    }
}

fn dispatch(cmd: &Cmd, nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    match cmd.cmd.as_str() {
        "ping" => Resp::ok(),
        "list_entries" => cmd_list_entries(nvs),
        "remove_entry" => cmd_remove_entry(cmd, nvs),
        "delete_entry" => cmd_delete_entry_by_slot(cmd, nvs),
        "sync_clock" => cmd_sync_clock(cmd, nvs),
        "factory_reset" => cmd_factory_reset(cmd, nvs),
        "allow_pairing" => {
            crate::ble_hid::OPEN_PAIRING_REQUESTED.store(true, Ordering::Relaxed);
            Resp::ok()
        }
        other => Resp::err(format!("unknown cmd: {other}")),
    }
}

// ── Command handlers ──────────────────────────────────────────────────────────

/// `add_entry` — finds a free slot, stores the secret + label in NVS, then
/// submits an [`EnrollRequest`] to the main loop and streams enrollment events
/// back over serial until enrollment completes or fails.
fn cmd_add_entry(
    uart: &UartDriver<'static>,
    cmd: &Cmd,
    nvs: &Arc<Mutex<SharedNvs>>,
    enroll_queue: &EnrollQueue,
) {
    let label = match cmd.label.as_deref().filter(|s| !s.is_empty()) {
        Some(l) => l,
        None => return write_resp(uart, &Resp::err("missing field: label")),
    };
    let secret = match cmd.secret_b32.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return write_resp(uart, &Resp::err("missing field: secret_b32")),
    };

    // Find the first free slot (0–9) and write secret + label to NVS.
    let slot: u32 = {
        let mut guard = nvs.lock().unwrap();
        let mut probe = [0u8; 65];
        let free = (0u32..10).find(|&s| {
            !matches!(
                guard.0.get_str(&format!("slot_{s}"), &mut probe),
                Ok(Some(_))
            )
        });
        let Some(s) = free else {
            return write_resp(uart, &Resp::err("no free slot"));
        };
        if let Err(e) = guard.0.set_str(&format!("slot_{s}"), secret) {
            return write_resp(uart, &Resp::err(format!("nvs error: {e}")));
        }
        if let Err(e) = guard.0.set_str(&format!("label_{s}"), label) {
            let _ = guard.0.remove(&format!("slot_{s}"));
            return write_resp(uart, &Resp::err(format!("nvs error: {e}")));
        }
        s
    };

    // Hand the enrollment request to the main loop.
    let (tx, rx) = mpsc::sync_channel(16);
    *enroll_queue.lock().unwrap() = Some(EnrollRequest {
        slot: slot as u16,
        reply: tx,
    });

    // Stream enrollment progress events over serial until done or failed.
    loop {
        match rx.recv() {
            Ok(EnrollResp::PlaceFinger { step, total }) => {
                write_event(
                    uart,
                    &EnrollEvent {
                        event: "enroll_step",
                        step,
                        total,
                        state: "place_finger",
                    },
                );
            }
            Ok(EnrollResp::LiftFinger { step, total }) => {
                write_event(
                    uart,
                    &EnrollEvent {
                        event: "enroll_step",
                        step,
                        total,
                        state: "lift_finger",
                    },
                );
            }
            Ok(EnrollResp::Done) => {
                write_resp(uart, &Resp::ok_slot(slot as u8));
                break;
            }
            Ok(EnrollResp::Failed) | Err(_) => {
                // Undo NVS writes so the slot is available for a retry.
                let mut guard = nvs.lock().unwrap();
                let _ = guard.0.remove(&format!("slot_{slot}"));
                let _ = guard.0.remove(&format!("label_{slot}"));
                write_resp(uart, &Resp::err("enrollment failed"));
                break;
            }
        }
    }
}

fn cmd_list_entries(nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    let guard = nvs.lock().unwrap();
    let mut entries = Vec::new();
    let mut secret_buf = [0u8; 65];
    let mut label_buf = [0u8; 33];
    for slot in 0u32..10 {
        if let Ok(Some(secret)) = guard.0.get_str(&format!("slot_{slot}"), &mut secret_buf) {
            let label = match guard.0.get_str(&format!("label_{slot}"), &mut label_buf) {
                Ok(Some(l)) => l.to_string(),
                _ => format!("slot {slot}"),
            };
            let visible: String = secret.chars().take(4).collect();
            let secret_masked = format!("{}{}", visible, "*".repeat(20));
            entries.push(SlotEntry {
                slot: slot as u8,
                label,
                secret_masked,
            });
        }
    }
    Resp { ok: true, error: None, entries: Some(entries), slot: None }
}

/// `remove_entry` — removes an entry by its service label (case-sensitive).
fn cmd_remove_entry(cmd: &Cmd, nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    let Some(label) = cmd.label.as_deref().filter(|s| !s.is_empty()) else {
        return Resp::err("missing field: label");
    };
    let mut guard = nvs.lock().unwrap();
    let mut label_buf = [0u8; 33];
    for slot in 0u32..10 {
        if let Ok(Some(stored)) = guard.0.get_str(&format!("label_{slot}"), &mut label_buf) {
            if stored == label {
                let _ = guard.0.remove(&format!("label_{slot}"));
                let _ = guard.0.remove(&format!("slot_{slot}"));
                return Resp::ok();
            }
        }
    }
    Resp::err(format!("entry '{label}' not found"))
}

/// `delete_entry` — legacy slot-based removal kept for backward compatibility.
fn cmd_delete_entry_by_slot(cmd: &Cmd, nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    let Some(slot) = cmd.slot else {
        return Resp::err("missing field: slot");
    };
    let mut guard = nvs.lock().unwrap();
    let _ = guard.0.remove(&format!("label_{slot}"));
    match guard.0.remove(&format!("slot_{slot}")) {
        Ok(true) => Resp::ok(),
        Ok(false) => Resp::err(format!("slot {slot} not found")),
        Err(e) => Resp::err(format!("nvs error: {e}")),
    }
}

fn cmd_sync_clock(cmd: &Cmd, nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    // Accept both "timestamp" (cyberkey-cli) and "ts" (legacy) field names.
    let Some(ts) = cmd.timestamp.or(cmd.ts) else {
        return Resp::err("missing field: timestamp");
    };
    let tv = esp_idf_svc::sys::timeval {
        tv_sec: ts as _,
        tv_usec: 0,
    };
    // Safety: settimeofday is always safe to call; null timezone = UTC.
    unsafe {
        esp_idf_svc::sys::settimeofday(&tv, core::ptr::null());
    }
    // Signal the main loop to write this timestamp to the BM8563 hardware
    // so the correct time survives a reboot.
    if let Ok(mut guard) = crate::rtc::PENDING_RTC_WRITE.lock() {
        *guard = Some(ts);
    }
    let offset = cmd.tz_offset_secs.unwrap_or(0);
    crate::rtc::UTC_OFFSET_SECS.store(offset, std::sync::atomic::Ordering::Relaxed);
    // Persist the offset so it is restored at the next boot without a host sync.
    if let Ok(guard) = nvs.lock() {
        let _ = guard.0.set_i32("tz_offset", offset);
    }
    log::info!("CLI: system clock set to {ts}, UTC offset {offset} s");
    Resp::ok()
}

fn cmd_factory_reset(cmd: &Cmd, nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    if cmd.confirm.as_deref() != Some("RESET") {
        return Resp::err("send confirm=\"RESET\" to confirm");
    }
    {
        let mut guard = nvs.lock().unwrap();
        for slot in 0u32..10 {
            let _ = guard.0.remove(&format!("slot_{slot}"));
            let _ = guard.0.remove(&format!("label_{slot}"));
        }
    }
    log::warn!(
        "CLI: factory reset — NVS erased, signalling main loop to clear fingerprints and reboot"
    );
    FACTORY_RESET.store(true, Ordering::Relaxed);
    Resp::ok()
}
