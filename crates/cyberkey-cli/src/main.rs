//! `cyberkey-cli` — Desktop configuration tool for the CyberKey TOTP authenticator.
//!
//! ## Usage
//!
//! ```text
//! # Auto-detect the first USB serial port
//! cyberkey-cli
//!
//! # Explicit port
//! cyberkey-cli --port /dev/tty.usbserial-3
//!
//! # Custom baud rate (defaults to 115 200)
//! cyberkey-cli --port /dev/ttyUSB0 --baud 9600
//! ```
//!
//! On launch the tool:
//! 1. Resolves the serial port (auto-detect or `--port`).
//! 2. Opens the port and attempts to read the firmware greeting.
//! 3. Syncs the device clock with the host Unix timestamp.
//! 4. Enters the interactive [`menu`] loop.

mod device;
mod display;
mod menu;
mod protocol;

use anyhow::{Context, Result};
use clap::Parser;
use dialoguer::{Select, theme::ColorfulTheme};

use crate::protocol::{Command, DeviceMessage};

// ── CLI arguments ─────────────────────────────────────────────────────────────

/// CyberKey Configuration Tool
///
/// Manage TOTP entries and enrolled fingerprints on your CyberKey device over
/// USB serial.
#[derive(Parser)]
#[command(name = "cyberkey-cli", version, about, long_about = None)]
struct Cli {
    /// Serial port path.
    ///
    /// If not specified, the first USB serial port detected by the OS is used
    /// automatically. If multiple ports are found you will be prompted to
    /// choose one.
    #[arg(short, long, value_name = "PORT")]
    port: Option<String>,

    /// Serial baud rate.
    #[arg(short, long, default_value_t = 115_200, value_name = "BAUD")]
    baud: u32,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Cli::parse();

    // ── Resolve port ──────────────────────────────────────────────────────────

    let port_path = match args.port {
        Some(p) => p,
        None => resolve_port_interactively()?,
    };

    // ── Connect ───────────────────────────────────────────────────────────────

    let mut device = device::Device::open(&port_path, args.baud)
        .with_context(|| format!("Cannot open serial port {port_path:?}"))?;

    // ── Read firmware greeting (non-blocking, best-effort) ────────────────────

    let firmware_version = device
        .try_read_greeting()
        .unwrap_or_else(|_| "unknown".to_string());

    println!();
    println!(
        "  Connected to CyberKey on {} (firmware {})",
        device.port_name, firmware_version
    );
    println!();

    // ── Auto-sync the device clock ────────────────────────────────────────────

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the UNIX epoch")?
        .as_secs();

    let tz_offset_secs = chrono::Local::now().offset().local_minus_utc();
    match device.call(&Command::SyncClock {
        timestamp: now,
        tz_offset_secs,
    }) {
        Ok(_) => println!("  Clock synced with host (t = {now}, UTC offset {tz_offset_secs:+}s)."),
        Err(e) => eprintln!("  Warning: clock sync failed — {e}"),
    }

    println!();

    // ── Authenticate the CLI session ──────────────────────────────────────────

    println!("  Place your finger on the sensor to authenticate...");
    match device.unlock()? {
        DeviceMessage::Ok => println!("  ✓ Authenticated."),
        DeviceMessage::Error { error } => anyhow::bail!("Authentication failed: {error}"),
        other => anyhow::bail!("Unexpected response from unlock: {other:?}"),
    }

    println!();

    // ── Run the interactive menu ───────────────────────────────────────────────

    menu::run(&mut device)?;

    println!("  Bye!");
    Ok(())
}

// ── Port resolution ───────────────────────────────────────────────────────────

/// Resolves the serial port path without a `--port` flag.
///
/// - **Zero USB ports**: bails with a helpful message.
/// - **One USB port**: uses it automatically, printing the chosen path.
/// - **Multiple USB ports**: shows an interactive `Select` prompt.
fn resolve_port_interactively() -> Result<String> {
    let candidates = device::list_usb_ports();

    match candidates.len() {
        0 => anyhow::bail!(
            "No USB serial port detected.\n\
             Connect the CyberKey device via USB and try again, or specify a port \
             manually with --port PATH."
        ),

        1 => {
            let port = candidates.into_iter().next().unwrap();
            println!("  Auto-detected port: {port}");
            Ok(port)
        }

        _ => {
            let idx = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Multiple USB serial ports found — select one")
                .items(&candidates)
                .default(0)
                .interact()
                .context("port selection cancelled")?;
            Ok(candidates.into_iter().nth(idx).unwrap())
        }
    }
}
