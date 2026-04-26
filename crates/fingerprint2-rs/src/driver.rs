//! Blocking UART driver for the M5Stack Unit Fingerprint2 (U203).
//!
//! # Design
//!
//! The driver is generic over any UART implementation that satisfies
//! [`embedded_hal_nb::serial::Read`] and [`embedded_hal_nb::serial::Write`].
//! In production firmware the real `esp-idf-hal` UART peripheral is passed in.
//! In unit tests a [`MockUart`](tests::MockUart) backed by in-memory buffers is
//! used instead — no hardware required.
//!
//! All public methods except [`Fingerprint2Driver::poll_event`] are **blocking**:
//! they spin internally (up to [`MAX_RETRIES`] times) waiting for each byte.
//! `poll_event` is the single non-blocking method and returns
//! `Err(nb::Error::WouldBlock)` immediately when no data is available.

use heapless::Vec;

use crate::commands::{
    AutoEnrollFlags, LedColor, LedMode, PS_ACTIVATE, PS_AUTO_ENROLL, PS_AUTO_IDENTIFY,
    PS_CONTROL_BLN, PS_DELET_CHAR, PS_HANDSHAKE,
};
use crate::error::FingerprintError;
use crate::packet::{self, DEFAULT_ADDR, Frame, MAX_DATA_LEN, PacketType};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of `WouldBlock` retries before [`Fingerprint2Driver::read_byte`]
/// gives up and returns [`FingerprintError::Timeout`].
const MAX_RETRIES: usize = 50_000_000;

/// Maximum serialized frame size in bytes.
/// = header(9) + MAX_DATA_LEN + checksum(2)
const MAX_FRAME_SIZE: usize = 9 + MAX_DATA_LEN + 2;

// ---------------------------------------------------------------------------
// DriverEvent
// ---------------------------------------------------------------------------

/// Events returned by [`Fingerprint2Driver::poll_event`].
#[derive(Debug, PartialEq)]
pub enum DriverEvent {
    /// The sensor woke autonomously (a finger was placed on the pad).
    ///
    /// Call [`Fingerprint2Driver::auto_identify`] next to identify the finger.
    Wakeup,

    /// An unsolicited ACK frame arrived (uncommon; captured for diagnostics).
    Ack {
        confirm: u8,
        data: heapless::Vec<u8, { crate::packet::MAX_DATA_LEN }>,
    },
}

// ---------------------------------------------------------------------------
// Fingerprint2Driver
// ---------------------------------------------------------------------------

/// Blocking UART driver for the Fingerprint2 sensor.
///
/// `UART` must implement [`embedded_hal_nb::serial::Read<u8>`],
/// [`embedded_hal_nb::serial::Write<u8>`], and
/// [`embedded_hal_nb::serial::ErrorType`] from `embedded-hal-nb` v1.
pub struct Fingerprint2Driver<UART> {
    /// The underlying UART peripheral (or mock in tests).
    uart: UART,
    /// Device address sent in every outgoing frame. Normally `0xFFFF_FFFF`.
    address: u32,
}

// ---------------------------------------------------------------------------
// impl — no UART bounds needed just to construct the struct
// ---------------------------------------------------------------------------

impl<UART> Fingerprint2Driver<UART> {
    /// Create a new driver wrapping `uart`.
    ///
    /// Uses the default broadcast address (`0xFFFF_FFFF`).
    pub fn new(uart: UART) -> Self {
        Self {
            uart,
            address: DEFAULT_ADDR,
        }
    }
}

// ---------------------------------------------------------------------------
// impl — all methods that actually use the UART
// ---------------------------------------------------------------------------

impl<UART, E> Fingerprint2Driver<UART>
where
    E: embedded_hal_nb::serial::Error,
    UART: embedded_hal_nb::serial::Read<u8>
        + embedded_hal_nb::serial::Write<u8>
        + embedded_hal_nb::serial::ErrorType<Error = E>,
{
    // =======================================================================
    // Private UART byte-level helpers
    // =======================================================================

    /// Read one byte, spinning on `WouldBlock` until data arrives or
    /// [`MAX_RETRIES`] is exhausted.
    fn read_byte(&mut self) -> Result<u8, FingerprintError<E>> {
        let mut retries = MAX_RETRIES;
        loop {
            match embedded_hal_nb::serial::Read::read(&mut self.uart) {
                Ok(b) => return Ok(b),
                Err(nb::Error::WouldBlock) => {
                    if retries == 0 {
                        return Err(FingerprintError::Timeout);
                    }
                    retries -= 1;
                }
                Err(nb::Error::Other(e)) => return Err(FingerprintError::Uart(e)),
            }
        }
    }

    /// Write one byte, spinning on `WouldBlock`.
    fn write_byte(&mut self, b: u8) -> Result<(), FingerprintError<E>> {
        loop {
            match embedded_hal_nb::serial::Write::write(&mut self.uart, b) {
                Ok(()) => return Ok(()),
                Err(nb::Error::WouldBlock) => continue,
                Err(nb::Error::Other(e)) => return Err(FingerprintError::Uart(e)),
            }
        }
    }

    /// Flush the UART transmit buffer, spinning on `WouldBlock`.
    fn flush_uart(&mut self) -> Result<(), FingerprintError<E>> {
        loop {
            match embedded_hal_nb::serial::Write::flush(&mut self.uart) {
                Ok(()) => return Ok(()),
                Err(nb::Error::WouldBlock) => continue,
                Err(nb::Error::Other(e)) => return Err(FingerprintError::Uart(e)),
            }
        }
    }

    // =======================================================================
    // Private frame-level helpers
    // =======================================================================

    /// Serialize `frame` and transmit every byte over UART, then flush.
    fn write_frame(&mut self, frame: &Frame) -> Result<(), FingerprintError<E>> {
        let mut buf = [0u8; MAX_FRAME_SIZE];
        let n = packet::serialize(frame, &mut buf).ok_or(FingerprintError::BadFrame)?;
        for &b in &buf[..n] {
            self.write_byte(b)?;
        }
        self.flush_uart()
    }

    /// Receive one complete frame from UART, validating the magic bytes and
    /// checksum as bytes arrive.
    fn read_frame(&mut self) -> Result<Frame, FingerprintError<E>> {
        // --- Magic (2 bytes) ------------------------------------------------
        let m0 = self.read_byte()?;
        let m1 = self.read_byte()?;
        if m0 != 0xEF || m1 != 0x01 {
            return Err(FingerprintError::BadFrame);
        }

        // --- Address (4 bytes, big-endian) ----------------------------------
        let a0 = self.read_byte()?;
        let a1 = self.read_byte()?;
        let a2 = self.read_byte()?;
        let a3 = self.read_byte()?;
        let addr = u32::from_be_bytes([a0, a1, a2, a3]);

        // --- Packet type (1 byte) -------------------------------------------
        let type_byte = self.read_byte()?;
        let packet_type =
            PacketType::try_from(type_byte).map_err(|_| FingerprintError::BadFrame)?;

        // --- LEN field (2 bytes, big-endian) --------------------------------
        // LEN = len(DATA) + 2   (the +2 counts the 2 checksum bytes)
        let len_hi = self.read_byte()?;
        let len_lo = self.read_byte()?;
        let len_field = u16::from_be_bytes([len_hi, len_lo]) as usize;

        if len_field < 2 {
            return Err(FingerprintError::BadFrame);
        }
        let data_len = len_field - 2;
        if data_len > MAX_DATA_LEN {
            return Err(FingerprintError::BadFrame);
        }

        // --- DATA bytes -----------------------------------------------------
        let mut data: Vec<u8, MAX_DATA_LEN> = Vec::new();
        for _ in 0..data_len {
            let b = self.read_byte()?;
            // push cannot fail: data_len <= MAX_DATA_LEN (checked above)
            data.push(b).map_err(|_| FingerprintError::BadFrame)?;
        }

        // --- Checksum (2 bytes, big-endian) ---------------------------------
        let cs_hi = self.read_byte()?;
        let cs_lo = self.read_byte()?;
        let received_csum = u16::from_be_bytes([cs_hi, cs_lo]);

        // CSUM = (TYPE + LEN + Σ DATA) & 0xFFFF
        let expected_csum = {
            let sum =
                type_byte as u32 + len_field as u32 + data.iter().map(|&b| b as u32).sum::<u32>();
            (sum & 0xFFFF) as u16
        };

        if received_csum != expected_csum {
            return Err(FingerprintError::BadChecksum);
        }

        Ok(Frame {
            addr,
            packet_type,
            data,
        })
    }

    // =======================================================================
    // Private command helpers
    // =======================================================================

    /// Build and transmit a [`PacketType::Command`] frame whose DATA section
    /// is `payload`.
    fn send_command(&mut self, payload: &[u8]) -> Result<(), FingerprintError<E>> {
        let mut data: Vec<u8, MAX_DATA_LEN> = Vec::new();
        for &b in payload {
            data.push(b).map_err(|_| FingerprintError::BadFrame)?;
        }
        self.write_frame(&Frame {
            addr: self.address,
            packet_type: PacketType::Command,
            data,
        })
    }

    /// Read one ACK frame and verify the confirmation code.
    ///
    /// Returns `Ok(frame)` when `DATA[0] == 0x00` (success).
    /// Maps any other confirmation code to the appropriate
    /// [`FingerprintError`] variant.
    fn read_ack(&mut self) -> Result<Frame, FingerprintError<E>> {
        let frame = self.read_frame()?;
        if frame.packet_type != PacketType::Ack {
            return Err(FingerprintError::BadFrame);
        }
        let confirm = frame.data.first().copied().unwrap_or(0xFF);
        if confirm != 0x00 {
            return Err(Self::map_confirm(confirm));
        }
        Ok(frame)
    }

    /// Map a non-zero sensor confirmation code to a [`FingerprintError`].
    ///
    /// | Code(s)               | Meaning                  | Mapped to         |
    /// |-----------------------|--------------------------|-------------------|
    /// | `0x08`, `0x09`        | No template match        | `NoMatch`         |
    /// | `0x03`,`0x06`,`0x07`,`0x0A` | Enrollment failure | `EnrollFailed`   |
    /// | anything else         | Undocumented sensor error| `SensorError(n)`  |
    fn map_confirm(code: u8) -> FingerprintError<E> {
        match code {
            0x08 | 0x09 => FingerprintError::NoMatch,
            0x03 | 0x06 | 0x07 | 0x0A => FingerprintError::EnrollFailed,
            other => FingerprintError::SensorError(other),
        }
    }

    /// Convert a codec error (which uses `Infallible` as the UART error) into
    /// the driver's generic [`FingerprintError<E>`].
    ///
    /// `packet::deserialize` only ever returns `BadFrame` or `BadChecksum`;
    /// all other variants hit `unreachable!`.
    fn convert_codec_err(e: FingerprintError<core::convert::Infallible>) -> FingerprintError<E> {
        match e {
            FingerprintError::BadFrame => FingerprintError::BadFrame,
            FingerprintError::BadChecksum => FingerprintError::BadChecksum,
            _ => unreachable!("packet::deserialize only returns BadFrame or BadChecksum"),
        }
    }

    // =======================================================================
    // Public API
    // =======================================================================

    /// Drain the UART RX buffer, discarding all pending bytes.
    ///
    /// Call before the first command to clear any unsolicited bytes the sensor
    /// may have emitted during its own boot sequence.
    pub fn drain_rx(&mut self) {
        while embedded_hal_nb::serial::Read::read(&mut self.uart).is_ok() {}
    }

    /// Activate the sensor.
    ///
    /// Required on cold boot for some firmware versions — the sensor responds
    /// to other commands with `0xFE` ("port inactive") until this is sent.
    pub fn activate(&mut self) -> Result<(), FingerprintError<E>> {
        self.send_command(&[PS_ACTIVATE])?;
        self.read_ack()?;
        Ok(())
    }

    /// Verify that the module is powered on and responsive.
    ///
    /// Sends a `PS_HANDSHAKE` command and checks for a success ACK.
    pub fn handshake(&mut self) -> Result<(), FingerprintError<E>> {
        self.send_command(&[PS_HANDSHAKE])?;
        self.read_ack()?;
        Ok(())
    }

    /// High-level autonomous enrollment.
    ///
    /// Sends one `PS_AUTO_ENROLL` command and then reads `count` ACK frames —
    /// the sensor emits one ACK per capture pass, prompting the user to
    /// lift and re-place the finger between passes. Returns `Ok(())` only if
    /// every capture pass succeeds.
    ///
    /// # Parameters
    ///
    /// - `id` — page ID in the sensor flash where the template will be stored.
    /// - `count` — number of capture passes (typically 3–6).
    /// - `flags` — whether to overwrite an already-occupied slot.
    pub fn auto_enroll(
        &mut self,
        id: u16,
        count: u8,
        flags: AutoEnrollFlags,
    ) -> Result<(), FingerprintError<E>> {
        let [id_hi, id_lo] = id.to_be_bytes();
        self.send_command(&[PS_AUTO_ENROLL, id_hi, id_lo, count, 0x00, flags.as_byte()])?;
        self.read_ack()?; // Consume immediate command ACK

        for _ in 0..count {
            self.read_ack()?;
        }
        Ok(())
    }

    /// Send `PS_AUTO_ENROLL` without reading any ACK.
    ///
    /// Use [`read_enroll_pass`] once per capture pass to receive each per-pass
    /// ACK non-blockingly, so the caller can update a display between passes.
    pub fn begin_auto_enroll(
        &mut self,
        id: u16,
        count: u8,
        flags: AutoEnrollFlags,
    ) -> Result<(), FingerprintError<E>> {
        let [id_hi, id_lo] = id.to_be_bytes();
        self.send_command(&[PS_AUTO_ENROLL, id_hi, id_lo, count, 0x00, flags.as_byte()])?;
        self.read_ack().map(|_| ())
    }

    /// Poll for one enrollment-pass ACK — **non-blocking**.
    ///
    /// Returns `Ok(())` on a success ACK, `Err(nb::Error::WouldBlock)` when no
    /// byte is available yet, or `Err(nb::Error::Other(...))` on sensor error.
    pub fn read_enroll_pass(
        &mut self,
    ) -> nb::Result<heapless::Vec<u8, MAX_DATA_LEN>, FingerprintError<E>> {
        match self.poll_event()? {
            DriverEvent::Ack { confirm: 0, data } => Ok(data),
            DriverEvent::Ack { confirm, .. } => Err(nb::Error::Other(Self::map_confirm(confirm))),
            DriverEvent::Wakeup => Err(nb::Error::Other(FingerprintError::BadFrame)),
        }
    }

    /// High-level autonomous identification.
    ///
    /// Sends one `PS_AUTO_IDENTIFY` command and waits for a single ACK that
    /// contains the matched page ID. Returns `Err(NoMatch)` when no enrolled
    /// template matches the placed finger.
    ///
    /// # Parameters
    ///
    /// - `security_level` — match threshold (1 = most permissive, 5 = strictest).
    ///
    /// Returns `Ok((page_id, score))` on a match.
    pub fn auto_identify(&mut self, security_level: u8) -> Result<(u16, u16), FingerprintError<E>> {
        // U203 protocol requires: [opcode, level, start_page_hi, start_page_lo, capacity_hi, capacity_lo].
        // Start 0 (0x0000), Capacity 200 (0x00C8) = search all 200 slots.
        self.send_command(&[PS_AUTO_IDENTIFY, security_level, 0x00, 0x00, 0x00, 0xC8])?;

        // The sensor sends a stream of stage-coded ACKs before the final result:
        //   data[1] = 0x00  LEGAL_CHECK  — discard
        //   data[1] = 0x01  GET_IMAGE    — discard
        //   data[1] = 0x05  VERIFY       — final: contains page_id + score
        // Loop until we reach the VERIFY stage; read_ack() propagates any
        // non-zero confirm (e.g. 0x09 NoMatch) as an error immediately.
        loop {
            let frame = self.read_ack()?;
            let stage = frame.data.get(1).copied().unwrap_or(0);
            log::info!("Stage reçu : {}", stage);

            if stage == 0x05 {
                // VERIFY ACK: [0x00, 0x05, id_hi, id_lo, score_hi, score_lo]
                if frame.data.len() < 4 {
                    return Err(FingerprintError::BadFrame);
                }
                let page_id = u16::from_be_bytes([frame.data[2], frame.data[3]]);
                let score = if frame.data.len() >= 6 {
                    u16::from_be_bytes([frame.data[4], frame.data[5]])
                } else {
                    0
                };
                return Ok((page_id, score));
            }
            // Intermediate stage (LEGAL_CHECK, GET_IMAGE) — keep reading.
        }
    }

    /// Control the RGB LED ring.
    ///
    /// # Parameters
    ///
    /// - `mode` — animation style (breathing, flashing, solid on/off, etc.).
    /// - `color` — LED colour.
    /// - `loops` — number of animation cycles; `0` usually means infinite.
    pub fn set_led(
        &mut self,
        mode: LedMode,
        color: LedColor,
        loops: u8,
    ) -> Result<(), FingerprintError<E>> {
        self.send_command(&[PS_CONTROL_BLN, mode as u8, color as u8, loops])?;
        self.read_ack()?;
        Ok(())
    }

    /// Check if a finger is currently on the pad (useful for active polling).
    ///
    /// Returns `Ok(())` if a finger is detected and an image was captured.
    /// Returns `Err(SensorError(2))` if no finger is on the pad.
    pub fn get_image(&mut self) -> Result<(), FingerprintError<E>> {
        self.send_command(&[crate::commands::PS_GET_IMAGE])?;
        self.read_ack()?;
        Ok(())
    }

    /// Delete one or more stored templates starting at `page_id`.
    ///
    /// To delete a single template pass `count = 1`.
    pub fn delete_template(&mut self, page_id: u16, count: u16) -> Result<(), FingerprintError<E>> {
        let [pid_hi, pid_lo] = page_id.to_be_bytes();
        let [cnt_hi, cnt_lo] = count.to_be_bytes();
        self.send_command(&[PS_DELET_CHAR, pid_hi, pid_lo, cnt_hi, cnt_lo])?;
        self.read_ack()?;
        Ok(())
    }

    /// Poll for an unsolicited incoming frame — **non-blocking**.
    ///
    /// - Returns `Ok(DriverEvent::Wakeup)` when the sensor's 12-byte autonomous
    ///   wakeup sequence is received (finger placed on pad).
    /// - Returns `Ok(DriverEvent::Ack { confirm })` for any other unsolicited
    ///   ACK frame.
    /// - Returns `Err(nb::Error::WouldBlock)` **immediately** when no byte is
    ///   available, so the firmware main-loop can sleep or handle BLE events
    ///   between polls without spinning.
    pub fn poll_event(&mut self) -> nb::Result<DriverEvent, FingerprintError<E>> {
        // Step 1 ─ non-blocking read of the very first byte.
        // If the rx buffer is empty we return WouldBlock immediately.
        let b0 = match embedded_hal_nb::serial::Read::read(&mut self.uart) {
            Ok(b) => b,
            Err(nb::Error::WouldBlock) => return Err(nb::Error::WouldBlock),
            Err(nb::Error::Other(e)) => return Err(nb::Error::Other(FingerprintError::Uart(e))),
        };

        // Step 2 ─ we are now committed to reading a full frame.
        // Read 11 more bytes (blocking, with timeout) to fill a 12-byte window.
        // This covers the wakeup packet and every minimal ACK frame.
        let mut buf12 = [0u8; 12];
        buf12[0] = b0;
        for slot in &mut buf12[1..] {
            *slot = self.read_byte().map_err(nb::Error::Other)?;
        }

        // Step 3 ─ wakeup check comes FIRST, before any other parsing.
        if packet::is_wakeup_packet(&buf12) {
            return Ok(DriverEvent::Wakeup);
        }

        // Step 4 ─ not a wakeup; validate magic bytes.
        if buf12[0] != 0xEF || buf12[1] != 0x01 {
            return Err(nb::Error::Other(FingerprintError::BadFrame));
        }

        // Step 5 ─ determine the total frame length from the LEN field.
        let len_field = u16::from_be_bytes([buf12[7], buf12[8]]) as usize;
        if len_field < 2 {
            return Err(nb::Error::Other(FingerprintError::BadFrame));
        }
        let total = 9 + len_field; // header(9) + data + checksum
        if total > MAX_FRAME_SIZE {
            return Err(nb::Error::Other(FingerprintError::BadFrame));
        }

        // Step 6 ─ assemble the complete frame in a stack buffer.
        let mut frame_buf = [0u8; MAX_FRAME_SIZE];
        if total <= 12 {
            // We already have all the bytes we need.
            frame_buf[..total].copy_from_slice(&buf12[..total]);
        } else {
            // Need more bytes beyond the initial 12.
            frame_buf[..12].copy_from_slice(&buf12);
            for slot in &mut frame_buf[12..total] {
                *slot = self.read_byte().map_err(nb::Error::Other)?;
            }
        }

        // Step 7 ─ parse and validate via the packet codec.
        let frame = packet::deserialize(&frame_buf[..total])
            .map_err(|e| nb::Error::Other(Self::convert_codec_err(e)))?;

        let confirm = frame.data.first().copied().unwrap_or(0);
        Ok(DriverEvent::Ack {
            confirm,
            data: frame.data,
        })
    }
}

// ---------------------------------------------------------------------------
// Unit / integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    // Shadow the heapless::Vec that the parent module imports so that bare
    // `Vec<u8>` in test helpers refers to the standard heap-allocated Vec.
    use std::vec::Vec;

    use super::*;
    use crate::commands::{AutoEnrollFlags, LedColor, LedMode};
    use crate::commands::{PS_AUTO_ENROLL, PS_CONTROL_BLN, PS_DELET_CHAR, PS_HANDSHAKE};
    use crate::packet::{self, DEFAULT_ADDR, Frame, MAX_DATA_LEN, PacketType};

    // =========================================================================
    // MockUart — in-memory UART for tests
    // =========================================================================

    /// Fake UART backed by a `VecDeque` (rx) and a `Vec` (tx).
    ///
    /// - `rx` is pre-loaded with the bytes the "sensor" will return.
    /// - `tx` accumulates every byte the driver sends, so tests can inspect it.
    struct MockUart {
        rx: VecDeque<u8>,
        tx: std::vec::Vec<u8>,
    }

    impl MockUart {
        /// Empty rx — simulates a silent/unresponsive sensor.
        fn new() -> Self {
            Self {
                rx: VecDeque::new(),
                tx: Vec::new(),
            }
        }

        /// Pre-load `rx` with the given bytes.
        fn with_rx(bytes: impl IntoIterator<Item = u8>) -> Self {
            Self {
                rx: bytes.into_iter().collect(),
                tx: Vec::new(),
            }
        }
    }

    impl embedded_hal_nb::serial::ErrorType for MockUart {
        type Error = core::convert::Infallible;
    }

    impl embedded_hal_nb::serial::Read<u8> for MockUart {
        fn read(&mut self) -> nb::Result<u8, Self::Error> {
            // When the buffer is empty return WouldBlock — just like a real
            // UART with no incoming data.
            self.rx.pop_front().ok_or(nb::Error::WouldBlock)
        }
    }

    impl embedded_hal_nb::serial::Write<u8> for MockUart {
        fn write(&mut self, word: u8) -> nb::Result<(), Self::Error> {
            self.tx.push(word);
            Ok(())
        }
        fn flush(&mut self) -> nb::Result<(), Self::Error> {
            Ok(())
        }
    }

    // =========================================================================
    // Test helpers
    // =========================================================================

    /// Serialize an ACK frame with the given DATA bytes into a `Vec<u8>`.
    ///
    /// Used to pre-load `MockUart.rx` with the sensor's expected response.
    fn ack_bytes(data: &[u8]) -> Vec<u8> {
        let mut d = heapless::Vec::<u8, MAX_DATA_LEN>::new();
        for &b in data {
            d.push(b).unwrap();
        }
        let frame = Frame {
            addr: DEFAULT_ADDR,
            packet_type: PacketType::Ack,
            data: d,
        };
        let mut buf = [0u8; 128];
        let n = packet::serialize(&frame, &mut buf).unwrap();
        buf[..n].to_vec()
    }

    /// Extract the DATA bytes from a serialized Command frame stored in `tx`.
    ///
    /// Frame layout: magic(2) + addr(4) + type(1) + LEN(2) = 9-byte header,
    /// then DATA, then checksum(2).
    fn extract_tx_data(tx: &[u8]) -> Vec<u8> {
        let len_field = u16::from_be_bytes([tx[7], tx[8]]) as usize;
        let data_len = len_field - 2; // subtract the 2 checksum bytes
        tx[9..9 + data_len].to_vec()
    }

    // =========================================================================
    // handshake
    // =========================================================================

    /// Pre-loaded ACK with confirm=0x00 → Ok(()).
    /// Also verifies that the correct PS_HANDSHAKE opcode was transmitted.
    #[test]
    fn handshake_happy_path() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x00])));

        assert_eq!(driver.handshake(), Ok(()));
        assert_eq!(extract_tx_data(&driver.uart.tx), vec![PS_HANDSHAKE]);
    }

    /// Empty rx → retries exhausted → Timeout.
    #[test]
    fn handshake_no_response() {
        let mut driver = Fingerprint2Driver::new(MockUart::new());
        assert_eq!(driver.handshake(), Err(FingerprintError::Timeout));
    }

    // =========================================================================
    // auto_identify
    // =========================================================================

    /// confirm code 0x09 on any ACK → NoMatch (error propagated from read_ack).
    #[test]
    fn auto_identify_no_match() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x09])));
        assert_eq!(driver.auto_identify(3), Err(FingerprintError::NoMatch));
    }

    /// Sensor sends LEGAL_CHECK then VERIFY — intermediate ACK is skipped,
    /// ID and score are extracted from the VERIFY frame.
    #[test]
    fn auto_identify_success() {
        // LEGAL_CHECK: [confirm=0x00, stage=0x00]
        // VERIFY:      [confirm=0x00, stage=0x05, id_hi=0x00, id_lo=0x05, score_hi=0x00, score_lo=0xFA]
        // page_id = 5 (0x0005), score = 250 (0x00FA)
        let rx: Vec<u8> = ack_bytes(&[0x00, 0x00])
            .into_iter()
            .chain(ack_bytes(&[0x00, 0x05, 0x00, 0x05, 0x00, 0xFA]))
            .collect();
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(rx));

        assert_eq!(driver.auto_identify(3), Ok((5, 250)));
    }

    /// Sensor sends only the VERIFY frame (no intermediate ACKs) — still works.
    #[test]
    fn auto_identify_success_no_intermediate() {
        let rx = ack_bytes(&[0x00, 0x05, 0x00, 0x05, 0x00, 0xFA]);
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(rx));
        assert_eq!(driver.auto_identify(3), Ok((5, 250)));
    }

    // =========================================================================
    // set_led
    // =========================================================================

    /// Verifies the PS_CONTROL_BLN opcode and the correct mode/color/loops bytes
    /// are emitted into tx.
    #[test]
    fn set_led_encoding() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x00])));

        driver
            .set_led(LedMode::Breathing, LedColor::Blue, 3)
            .unwrap();

        // DATA must be: [PS_CONTROL_BLN, Breathing=1, Blue=1, loops=3]
        assert_eq!(
            extract_tx_data(&driver.uart.tx),
            vec![PS_CONTROL_BLN, 0x01, 0x01, 0x03]
        );
    }

    // =========================================================================
    // auto_enroll
    // =========================================================================

    /// count=3 → three consecutive success ACKs → Ok(()).
    /// Verifies PS_AUTO_ENROLL opcode and big-endian id/count/flags bytes.
    #[test]
    fn auto_enroll_happy_path() {
        // Need 1 command ACK + 3 capture ACKs = 4 ACKs total
        let rx: Vec<u8> = (0..4).flat_map(|_| ack_bytes(&[0x00])).collect();
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(rx));

        assert_eq!(
            driver.auto_enroll(
                1,
                3,
                AutoEnrollFlags {
                    allow_overwrite: false
                }
            ),
            Ok(())
        );
        // DATA must be: [PS_AUTO_ENROLL, id_hi=0x00, id_lo=0x01, count=3, param_hi=0x00, param_lo=0x00]
        assert_eq!(
            extract_tx_data(&driver.uart.tx),
            vec![PS_AUTO_ENROLL, 0x00, 0x01, 0x03, 0x00, 0x00]
        );
    }

    /// Confirm code 0x06 (image too noisy) on the first capture pass → EnrollFailed.
    #[test]
    fn auto_enroll_quality_failure() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x06])));
        assert_eq!(
            driver.auto_enroll(
                1,
                3,
                AutoEnrollFlags {
                    allow_overwrite: false
                }
            ),
            Err(FingerprintError::EnrollFailed)
        );
    }

    // =========================================================================
    // delete_template
    // =========================================================================

    /// Verifies PS_DELET_CHAR opcode and big-endian page_id / count bytes.
    #[test]
    fn delete_template_encoding() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x00])));

        driver.delete_template(5, 1).unwrap();

        // DATA must be: [PS_DELET_CHAR, pid_hi=0x00, pid_lo=0x05, cnt_hi=0x00, cnt_lo=0x01]
        assert_eq!(
            extract_tx_data(&driver.uart.tx),
            vec![PS_DELET_CHAR, 0x00, 0x05, 0x00, 0x01]
        );
    }

    // =========================================================================
    // activate
    // =========================================================================

    /// ACK confirm=0x00 → Ok(()); verifies PS_ACTIVATE opcode is transmitted.
    #[test]
    fn activate_happy_path() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x00])));
        assert_eq!(driver.activate(), Ok(()));
        assert_eq!(extract_tx_data(&driver.uart.tx), vec![PS_ACTIVATE]);
    }

    /// Empty rx → Timeout.
    #[test]
    fn activate_no_response() {
        let mut driver = Fingerprint2Driver::new(MockUart::new());
        assert_eq!(driver.activate(), Err(FingerprintError::Timeout));
    }

    // =========================================================================
    // Confirm code mapping
    // =========================================================================

    /// Unmapped confirm code (0x15 = wrong password) → SensorError(0x15).
    #[test]
    fn unmapped_sensor_error_propagated() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x15])));
        assert_eq!(driver.handshake(), Err(FingerprintError::SensorError(0x15)));
    }

    // =========================================================================
    // poll_event
    // =========================================================================

    /// Pre-loading the 12-byte wakeup sequence → Ok(DriverEvent::Wakeup).
    #[test]
    fn poll_event_wakeup() {
        let wakeup = vec![
            0xEF_u8, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x07, 0x00, 0x03, 0xFF, 0x01, 0x09,
        ];
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(wakeup));
        assert_eq!(driver.poll_event(), Ok(DriverEvent::Wakeup));
    }

    /// Empty rx → WouldBlock returned immediately (no retries, no timeout).
    #[test]
    fn poll_event_no_data() {
        let mut driver = Fingerprint2Driver::new(MockUart::new());
        assert_eq!(driver.poll_event(), Err(nb::Error::WouldBlock));
    }
}
