//! `device` — serial port connection to the CyberKey firmware.
//!
//! This module owns the raw `serialport` handle and delegates all message
//! encoding/decoding to [`crate::protocol`]. The goal is to keep I/O concerns
//! here and keep the protocol module purely functional so it remains testable
//! without hardware.
//!
//! ## Usage
//!
//! ```no_run
//! use cyberkey_cli::device::{Device, list_usb_ports};
//! use cyberkey_cli::protocol::Command;
//!
//! let port = list_usb_ports().into_iter().next().expect("no USB port found");
//! let mut dev = Device::open(&port, 115_200).unwrap();
//! let msg = dev.call(&Command::ListEntries).unwrap();
//! ```

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::protocol::{self, Command, DeviceMessage, EnrollState};

// ── Timeouts ──────────────────────────────────────────────────────────────────

/// Default read timeout for normal command/response exchanges.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Shorter timeout used when attempting to read an optional firmware greeting
/// on connect. If nothing arrives within this window the CLI continues without
/// a version string.
const GREETING_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Longer timeout for enrollment, which involves multiple physical interactions.
const ENROLL_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for the unlock command — the user needs to physically place their
/// finger within this window.
const UNLOCK_TIMEOUT: Duration = Duration::from_secs(30);

// ── Port discovery ────────────────────────────────────────────────────────────

/// Returns the names of all USB serial ports currently visible to the OS.
///
/// Filters `serialport::available_ports()` to USB-backed entries only, which
/// excludes built-in Bluetooth serial ports and other non-hardware noise.
/// Returns an empty vector if the `serialport` crate cannot enumerate ports
/// (e.g. missing permissions on Linux without `udev` rules).
pub fn list_usb_ports() -> Vec<String> {
    match serialport::available_ports() {
        Ok(ports) => ports
            .into_iter()
            .filter(|p| matches!(p.port_type, serialport::SerialPortType::UsbPort(_)))
            .map(|p| p.port_name)
            .collect(),
        Err(_) => vec![],
    }
}

// ── Device ────────────────────────────────────────────────────────────────────

/// An open, authenticated connection to the CyberKey firmware over USB serial.
///
/// Internally holds two handles to the same underlying port:
/// - `writer` — used exclusively for sending commands.
/// - `reader` — a [`BufReader`] used for line-by-line message reception.
///
/// Both handles were obtained from a single `serialport::open` call via
/// [`SerialPort::try_clone`], so they share the same OS file descriptor on
/// POSIX systems and the same `HANDLE` on Windows.
pub struct Device {
    /// Write-only handle (sends encoded commands).
    writer: Box<dyn serialport::SerialPort>,
    /// Buffered read handle (receives newline-delimited JSON messages).
    reader: BufReader<Box<dyn serialport::SerialPort>>,
    /// The OS path of the port, kept for informational display.
    pub port_name: String,
}

impl Device {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Opens the serial port at `port_path` with the given `baud_rate` and
    /// returns a [`Device`] ready for use.
    ///
    /// The port is configured with [`DEFAULT_TIMEOUT`]; individual operations
    /// that need a different timeout change it temporarily via
    /// [`serialport::SerialPort::set_timeout`].
    ///
    /// # Errors
    ///
    /// Propagates any error from the `serialport` crate (port not found,
    /// permission denied, already in use, etc.).
    pub fn open(port_path: &str, baud_rate: u32) -> Result<Self> {
        let port = serialport::new(port_path, baud_rate)
            .timeout(DEFAULT_TIMEOUT)
            .open()
            .with_context(|| format!("cannot open serial port {port_path:?}"))?;

        // Clone the handle before moving `port` into BufReader, so we keep a
        // separate writable handle.
        let writer = port
            .try_clone()
            .context("failed to clone serial port for writing")?;

        let reader = BufReader::new(port);

        Ok(Self {
            writer,
            reader,
            port_name: port_path.to_string(),
        })
    }

    // ── Greeting ──────────────────────────────────────────────────────────────

    /// Attempts to read the firmware greeting (`{"version":"v0.x.y"}`) that
    /// the device sends immediately after the serial connection is established.
    ///
    /// Uses a short [`GREETING_TIMEOUT`] so that the CLI does not hang if the
    /// device is already running (and therefore does not send a greeting) or if
    /// the firmware version that is installed does not implement greetings yet.
    ///
    /// Returns the version string on success, or an error if no greeting
    /// arrives within the timeout window.
    pub fn try_read_greeting(&mut self) -> Result<String> {
        // Temporarily lower the timeout so we do not block for 10 s.
        self.reader
            .get_mut()
            .set_timeout(GREETING_TIMEOUT)
            .context("failed to set greeting timeout")?;

        let result = self.recv();

        // Restore the default timeout regardless of whether the read succeeded.
        let _ = self.reader.get_mut().set_timeout(DEFAULT_TIMEOUT);

        match result? {
            DeviceMessage::Greeting { version } => Ok(version),
            other => anyhow::bail!("expected firmware greeting, got {other:?}"),
        }
    }

    // ── Low-level I/O ─────────────────────────────────────────────────────────

    /// Encodes `cmd` and writes the resulting bytes to the serial port.
    ///
    /// Flushes the OS write buffer before returning so the firmware receives
    /// the command immediately.
    ///
    /// # Errors
    ///
    /// - [`protocol::encode_command`] failure (practically unreachable).
    /// - OS write/flush error.
    pub fn send(&mut self, cmd: &Command) -> Result<()> {
        let bytes = protocol::encode_command(cmd)?;
        self.writer
            .write_all(&bytes)
            .context("serial write failed")?;
        self.writer.flush().context("serial flush failed")?;
        Ok(())
    }

    /// Reads the next JSON response line from the serial port.
    ///
    /// Firmware log lines (produced by EspLogger on the same UART) are
    /// silently skipped — only lines that start with `{` are decoded.
    ///
    /// Blocks until a JSON line arrives or the configured timeout expires.
    ///
    /// # Errors
    ///
    /// - Read timeout or OS I/O error.
    /// - JSON parse or structural decode error from [`protocol::decode_response`].
    pub fn recv(&mut self) -> Result<DeviceMessage> {
        loop {
            let mut line = String::new();
            self.reader
                .read_line(&mut line)
                .context("serial read failed")?;
            let trimmed = line.trim_end_matches(['\n', '\r']).trim();
            if trimmed.starts_with('{') {
                return protocol::decode_response(trimmed.as_bytes());
            }
            // Skip firmware log lines (e.g. "I (1234) cli: ...").
        }
    }

    // ── High-level request/response ───────────────────────────────────────────

    /// Sends `cmd` and waits for exactly one response message.
    ///
    /// This covers all commands that yield a single, immediate reply. For
    /// `add_entry` — which streams enrollment events before its final response
    /// — use [`Device::enroll`] instead.
    pub fn call(&mut self, cmd: &Command) -> Result<DeviceMessage> {
        self.send(cmd)?;
        self.recv()
    }

    // ── Unlock ────────────────────────────────────────────────────────────────

    /// Sends an `unlock` command and waits for the fingerprint authentication result.
    ///
    /// The read timeout is temporarily raised to [`UNLOCK_TIMEOUT`] to allow
    /// the user to physically place their finger on the sensor. It is restored
    /// after the command completes whether or not it succeeds.
    pub fn unlock(&mut self) -> Result<DeviceMessage> {
        self.reader
            .get_mut()
            .set_timeout(UNLOCK_TIMEOUT)
            .context("failed to set unlock timeout")?;

        let result = self.call(&Command::Unlock);

        let _ = self.reader.get_mut().set_timeout(DEFAULT_TIMEOUT);

        result
    }

    // ── Enrollment ────────────────────────────────────────────────────────────

    /// Sends an `add_entry` command and drives the enrollment protocol to
    /// completion, calling `on_step` for each intermediate capture event.
    ///
    /// ## Enrollment protocol
    ///
    /// After receiving `add_entry`, the firmware streams a series of
    /// `{"event":"enroll_step",...}` messages — one `place_finger` and one
    /// `lift_finger` per capture pass — before sending the terminal
    /// `{"ok":true,"slot":N}` or `{"ok":false,"error":"..."}` response.
    ///
    /// `on_step(step, total, &state)` is invoked synchronously for each event;
    /// callers can use it to render a progress bar or log to stdout.
    ///
    /// ## Timeout
    ///
    /// The read timeout is temporarily raised to [`ENROLL_TIMEOUT`] to
    /// accommodate the time the user needs to physically place and lift their
    /// finger between each pass. It is restored after enrollment completes
    /// (whether successfully or not).
    ///
    /// ## Errors
    ///
    /// - OS read/write error.
    /// - Firmware enrollment failure (`EnrollFailed`, `EnrollTimeout`, etc.).
    /// - Unexpected message type received during the enrollment sequence.
    pub fn enroll(
        &mut self,
        label: &str,
        secret_b32: &str,
        on_step: &mut impl FnMut(u8, u8, &EnrollState),
    ) -> Result<u8> {
        // Raise the timeout so user physical interactions don't time out.
        self.reader
            .get_mut()
            .set_timeout(ENROLL_TIMEOUT)
            .context("failed to set enrollment timeout")?;

        let result = self.enroll_inner(label, secret_b32, on_step);

        // Always restore the default timeout.
        let _ = self.reader.get_mut().set_timeout(DEFAULT_TIMEOUT);

        result
    }

    /// Inner enrollment logic, separated so the timeout restore in [`enroll`]
    /// is guaranteed even on early return.
    fn enroll_inner(
        &mut self,
        label: &str,
        secret_b32: &str,
        on_step: &mut impl FnMut(u8, u8, &EnrollState),
    ) -> Result<u8> {
        let cmd = Command::AddEntry {
            label: label.to_string(),
            secret_b32: secret_b32.to_string(),
        };
        self.send(&cmd)?;

        loop {
            match self.recv()? {
                DeviceMessage::EnrollStep { step, total, state } => {
                    on_step(step, total, &state);
                }
                DeviceMessage::AddEntryOk { slot } => return Ok(slot),
                DeviceMessage::Error { error } => {
                    anyhow::bail!("enrollment failed: {error}");
                }
                other => {
                    anyhow::bail!("unexpected message during enrollment: {other:?}");
                }
            }
        }
    }
}
