//! CLI wire protocol — listens on UART0 (USB-serial) for JSON newline-delimited commands.
//!
//! Each command is a JSON object with at minimum a `"cmd"` field, e.g.:
//!   {"cmd":"ping"}
//!   {"cmd":"list_entries"}
//!   {"cmd":"delete_entry","slot":0}
//!   {"cmd":"sync_clock","ts":1745000000}
//!   {"cmd":"factory_reset"}
//!
//! Each response is a JSON object on a single line followed by `\n`.
//! Log output from EspLogger is interleaved on the same UART; the host tool
//! should filter for lines that start with `{`.

use std::sync::{Arc, Mutex};

use esp_idf_svc::{
    hal::{delay::BLOCK, uart::UartDriver},
    nvs::{EspNvs, NvsDefault},
};
use serde::{Deserialize, Serialize};

/// Newtype that lets `EspNvs` cross thread boundaries under a `Mutex`.
///
/// ESP-IDF NVS handles are not inherently thread-safe, but the `Mutex` ensures
/// only one thread calls into the handle at a time.
pub struct SharedNvs(pub EspNvs<NvsDefault>);
// Safety: access is serialised by the surrounding Mutex.
unsafe impl Send for SharedNvs {}

// ── Wire types ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Cmd {
    cmd: String,
    slot: Option<u32>,
    ts: Option<u64>,
}

#[derive(Serialize)]
struct Resp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entries: Option<Vec<SlotEntry>>,
}

#[derive(Serialize)]
struct SlotEntry {
    slot: u32,
}

impl Resp {
    fn ok() -> Self {
        Resp {
            ok: true,
            msg: None,
            entries: None,
        }
    }
    fn ok_msg(msg: impl Into<String>) -> Self {
        Resp {
            ok: true,
            msg: Some(msg.into()),
            entries: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Resp {
            ok: false,
            msg: Some(msg.into()),
            entries: None,
        }
    }
}

// ── Task entry ───────────────────────────────────────────────────────────────

/// Spawn the CLI listener as a dedicated FreeRTOS task.
///
/// `uart` must be a `UartDriver<'static>` — the caller is responsible for
/// ensuring the underlying peripheral lives for the program duration (use
/// `core::mem::transmute` after init; the peripheral is always `'static` on
/// bare-metal).
pub fn spawn(uart: UartDriver<'static>, nvs: Arc<Mutex<SharedNvs>>) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || run(uart, nvs))?;
    Ok(())
}

fn run(uart: UartDriver<'static>, nvs: Arc<Mutex<SharedNvs>>) {
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut byte = [0u8; 1];

    loop {
        if uart.read(&mut byte, BLOCK).is_err() {
            continue;
        }
        match byte[0] {
            b'\n' => {
                if !buf.is_empty() {
                    let resp = process(&buf, &nvs);
                    buf.clear();
                    if let Ok(mut out) = serde_json::to_vec(&resp) {
                        out.push(b'\n');
                        let _ = uart.write(&out);
                    }
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

// ── Dispatch ─────────────────────────────────────────────────────────────────

fn process(raw: &[u8], nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    let s = match core::str::from_utf8(raw) {
        Ok(s) => s.trim(),
        Err(_) => return Resp::err("invalid utf-8"),
    };
    if s.is_empty() {
        return Resp::ok();
    }
    match serde_json::from_str::<Cmd>(s) {
        Ok(cmd) => dispatch(&cmd, nvs),
        Err(e) => Resp::err(format!("parse error: {e}")),
    }
}

fn dispatch(cmd: &Cmd, nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    match cmd.cmd.as_str() {
        "ping" => Resp::ok_msg("pong"),
        "list_entries" => cmd_list_entries(nvs),
        "delete_entry" => cmd_delete_entry(cmd, nvs),
        "sync_clock" => cmd_sync_clock(cmd),
        "factory_reset" => cmd_factory_reset(nvs),
        other => Resp::err(format!("unknown cmd: {other}")),
    }
}

// ── Command handlers ─────────────────────────────────────────────────────────

fn cmd_list_entries(nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    let guard = nvs.lock().unwrap();
    let mut entries = Vec::new();
    let mut probe = [0u8; 65];
    for slot in 0u32..10 {
        let key = format!("slot_{slot}");
        if matches!(guard.0.get_str(&key, &mut probe), Ok(Some(_))) {
            entries.push(SlotEntry { slot });
        }
    }
    Resp {
        ok: true,
        msg: None,
        entries: Some(entries),
    }
}

fn cmd_delete_entry(cmd: &Cmd, nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    let Some(slot) = cmd.slot else {
        return Resp::err("missing field: slot");
    };
    let key = format!("slot_{slot}");
    let mut guard = nvs.lock().unwrap();
    match guard.0.remove(&key) {
        Ok(true) => Resp::ok(),
        Ok(false) => Resp::err(format!("slot {slot} not found")),
        Err(e) => Resp::err(format!("nvs error: {e}")),
    }
}

fn cmd_sync_clock(cmd: &Cmd) -> Resp {
    let Some(ts) = cmd.ts else {
        return Resp::err("missing field: ts");
    };
    let tv = esp_idf_svc::sys::timeval {
        tv_sec: ts as _,
        tv_usec: 0,
    };
    // Safety: settimeofday is always safe to call; null timezone = UTC.
    unsafe {
        esp_idf_svc::sys::settimeofday(&tv, core::ptr::null());
    }
    log::info!("CLI: system clock set to {ts}");
    Resp::ok()
}

fn cmd_factory_reset(nvs: &Arc<Mutex<SharedNvs>>) -> Resp {
    {
        let mut guard = nvs.lock().unwrap();
        for slot in 0u32..10 {
            let key = format!("slot_{slot}");
            let _ = guard.0.remove(&key);
        }
    }
    log::warn!("CLI: factory reset — erased NVS, rebooting");
    // Safety: esp_restart() is always valid to call and does not return.
    unsafe { esp_idf_svc::sys::esp_restart() }
}
