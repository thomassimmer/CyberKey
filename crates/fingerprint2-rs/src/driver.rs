//! Blocking UART driver for the M5Stack Unit Fingerprint2 (U203).
//!
//! # Design
//!
//! The driver is generic over any UART implementation that satisfies
//! [`embedded_hal_nb::serial::Read`] and [`embedded_hal_nb::serial::Write`].
//! In production firmware the real `esp-idf-hal` UART peripheral is passed in.
//! In unit tests a `MockUart` backed by in-memory buffers is used instead —
//! no hardware required.
//!
//! All public methods are **blocking**: they spin internally (up to
//! `READ_TIMEOUT_MS` = 500 times, 1 ms each) waiting for each byte.

use heapless::Vec;

use crate::commands::{
    PS_ACTIVATE, PS_AUTO_IDENTIFY, PS_DELET_CHAR, PS_GEN_CHAR, PS_GET_ENROLL_IMAGE, PS_HANDSHAKE,
    PS_REG_MODEL, PS_SEARCH, PS_STORE_CHAR,
};
use crate::error::FingerprintError;
use crate::packet::{self, DEFAULT_ADDR, Frame, MAX_DATA_LEN, PacketType};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Milliseconds to wait (yielding 1 ms per attempt) before [`Fingerprint2Driver::read_byte`]
/// gives up and returns [`FingerprintError::Timeout`].
const READ_TIMEOUT_MS: u32 = 500;

/// Extended timeout for commands that write to the sensor's internal flash
/// (`PS_STORE_CHAR`). Writing a template to flash on the U203's STM32 MCU can
/// take over 500 ms; this value gives a comfortable margin.
pub const READ_TIMEOUT_FLASH_MS: u32 = 3000;

/// Maximum serialized frame size in bytes.
/// = header(9) + MAX_DATA_LEN + checksum(2)
const MAX_FRAME_SIZE: usize = 9 + MAX_DATA_LEN + 2;

// ---------------------------------------------------------------------------
// Fingerprint2Driver
// ---------------------------------------------------------------------------

/// Blocking UART driver for the Fingerprint2 sensor.
///
/// `UART` must implement [`embedded_hal_nb::serial::Read<u8>`],
/// [`embedded_hal_nb::serial::Write<u8>`], and
/// [`embedded_hal_nb::serial::ErrorType`] from `embedded-hal-nb` v1.
///
/// `DELAY` must implement [`embedded_hal::delay::DelayNs`]. On each
/// `WouldBlock` in `read_byte` the driver calls `delay.delay_ms(1)`, which
/// yields to the FreeRTOS scheduler instead of spinning.
pub struct Fingerprint2Driver<UART, DELAY> {
    /// The underlying UART peripheral (or mock in tests).
    uart: UART,
    /// Delay provider — yields the CPU on each WouldBlock retry.
    delay: DELAY,
    /// Device address sent in every outgoing frame. Normally `0xFFFF_FFFF`.
    address: u32,
}

// ---------------------------------------------------------------------------
// impl — no UART bounds needed just to construct the struct
// ---------------------------------------------------------------------------

impl<UART, DELAY> Fingerprint2Driver<UART, DELAY> {
    /// Create a new driver wrapping `uart`.
    ///
    /// `delay` is called with `delay_ms(1)` on every `WouldBlock` retry so
    /// that the FreeRTOS scheduler can run other tasks while waiting for a byte.
    /// Pass `esp_idf_svc::hal::delay::FreeRtos` in production firmware and a
    /// no-op delay in unit tests.
    ///
    /// Uses the default broadcast address (`0xFFFF_FFFF`).
    pub fn new(uart: UART, delay: DELAY) -> Self {
        Self {
            uart,
            delay,
            address: DEFAULT_ADDR,
        }
    }
}

// ---------------------------------------------------------------------------
// impl — all methods that actually use the UART
// ---------------------------------------------------------------------------

impl<UART, E, DELAY> Fingerprint2Driver<UART, DELAY>
where
    E: embedded_hal_nb::serial::Error,
    UART: embedded_hal_nb::serial::Read<u8>
        + embedded_hal_nb::serial::Write<u8>
        + embedded_hal_nb::serial::ErrorType<Error = E>,
    DELAY: embedded_hal::delay::DelayNs,
{
    // =======================================================================
    // Private UART byte-level helpers
    // =======================================================================

    /// Read one byte, yielding 1 ms to the scheduler on each `WouldBlock`
    /// until data arrives or `timeout_ms` milliseconds have elapsed.
    fn read_byte_timeout(&mut self, timeout_ms: u32) -> Result<u8, FingerprintError<E>> {
        for _ in 0..timeout_ms {
            match embedded_hal_nb::serial::Read::read(&mut self.uart) {
                Ok(b) => return Ok(b),
                Err(nb::Error::WouldBlock) => self.delay.delay_ms(1),
                Err(nb::Error::Other(e)) => return Err(FingerprintError::Uart(e)),
            }
        }
        Err(FingerprintError::Timeout)
    }

    /// Read one byte with the standard [`READ_TIMEOUT_MS`] deadline.
    fn read_byte(&mut self) -> Result<u8, FingerprintError<E>> {
        self.read_byte_timeout(READ_TIMEOUT_MS)
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

    /// Receive one complete frame from UART using a custom first-byte timeout.
    ///
    /// `first_byte_timeout_ms` controls how long to wait for the very first byte
    /// of the response. Subsequent bytes within the same frame always use
    /// [`READ_TIMEOUT_MS`] — once the sensor has started transmitting, bytes
    /// arrive quickly at 115 200 baud.
    fn read_frame_timeout(
        &mut self,
        first_byte_timeout_ms: u32,
    ) -> Result<Frame, FingerprintError<E>> {
        // --- Magic (2 bytes) — first byte uses the caller-supplied timeout ----
        let m0 = self.read_byte_timeout(first_byte_timeout_ms)?;
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
        self.read_ack_timeout(READ_TIMEOUT_MS)
    }

    /// Like [`read_ack`](Self::read_ack) but with a custom first-byte timeout.
    ///
    /// Use this for commands that trigger slow sensor-side operations (e.g.
    /// flash writes) where the ACK may not arrive within [`READ_TIMEOUT_MS`].
    fn read_ack_timeout(
        &mut self,
        first_byte_timeout_ms: u32,
    ) -> Result<Frame, FingerprintError<E>> {
        let frame = self.read_frame_timeout(first_byte_timeout_ms)?;
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

    /// High-level autonomous identification (1:N search across all enrolled templates).
    ///
    /// Sends one `PS_AUTO_IDENTIFY` command with `ID=0xFFFF` (full-library search)
    /// and reads the stream of stage ACKs until the VERIFY stage returns the matched
    /// page ID. Returns `Err(NoMatch)` when no enrolled template matches.
    ///
    /// # Parameters
    ///
    /// - `security_level` — match threshold (0 = most permissive, higher = stricter).
    ///
    /// Returns `Ok((page_id, score))` on a match.
    pub fn auto_identify(&mut self, security_level: u8) -> Result<(u16, u16), FingerprintError<E>> {
        // U203 protocol: [opcode, level, id_hi, id_lo, flags_hi, flags_lo].
        // ID=0xFFFF is the sentinel for a full 1:N identification across all enrolled templates.
        // Any specific ID (e.g. 0x0000) triggers a 1:1 verification against that single slot only.
        self.send_command(&[PS_AUTO_IDENTIFY, security_level, 0xFF, 0xFF, 0x00, 0x00])?;

        // The sensor sends a stream of stage-coded ACKs before the final result:
        //   data[1] = 0x00  LEGAL_CHECK  — discard
        //   data[1] = 0x01  GET_IMAGE    — discard
        //   data[1] = 0x05  VERIFY       — final: contains page_id + score
        // Loop until we reach the VERIFY stage; read_ack() propagates any
        // non-zero confirm (e.g. 0x09 NoMatch) as an error immediately.
        loop {
            let frame = self.read_ack()?;
            let stage = frame.data.get(1).copied().unwrap_or(0);

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

    /// Check if a finger is currently on the pad (useful for active polling).
    ///
    /// Returns `Ok(())` if a finger is detected and an image was captured.
    /// Returns `Err(SensorError(2))` if no finger is on the pad.
    pub fn get_image(&mut self) -> Result<(), FingerprintError<E>> {
        self.send_command(&[crate::commands::PS_GET_IMAGE])?;
        self.read_ack()?;
        Ok(())
    }

    /// Capture a finger image optimised for enrollment quality (`PS_GET_ENROLL_IMAGE`, 0x29).
    ///
    /// Drop-in equivalent of [`get_image`](Self::get_image) but uses the enrollment-specific
    /// opcode. The sensor firmware may apply different quality thresholds internally.
    /// Always call this — not `get_image` — during manual enrollment passes.
    ///
    /// Returns `Ok(())` on a successful capture, `Err(SensorError(2))` when no finger
    /// is present.
    pub fn get_enroll_image(&mut self) -> Result<(), FingerprintError<E>> {
        self.send_command(&[PS_GET_ENROLL_IMAGE])?;
        self.read_ack()?;
        Ok(())
    }

    /// Extract features from the last captured image into a character buffer (`PS_GEN_CHAR`, 0x02).
    ///
    /// Must be called immediately after a successful [`get_enroll_image`](Self::get_enroll_image)
    /// or [`get_image`](Self::get_image). The resulting feature set is written into the
    /// sensor's internal CharBuffer identified by `buf` (1 or 2).
    ///
    /// During a 3-pass enrollment the buffer alternates: pass 1 → buf 1, pass 2 → buf 2,
    /// pass 3 → buf 1 (overwrites with the highest-quality capture). After all passes,
    /// CharBuffer 1 and CharBuffer 2 hold complementary feature sets ready for
    /// [`reg_model`](Self::reg_model).
    pub fn gen_char(&mut self, buf: u8) -> Result<(), FingerprintError<E>> {
        self.send_command(&[PS_GEN_CHAR, buf])?;
        self.read_ack()?;
        Ok(())
    }

    /// Search the template library for a match against a character buffer (`PS_SEARCH`, 0x04).
    ///
    /// Compares the feature set in CharBuffer `buf` against all stored templates in
    /// the range `[start_page, start_page + page_count)`. No finger placement is required;
    /// the buffer must already be populated by a prior [`gen_char`](Self::gen_char) call.
    ///
    /// Returns `Ok((page_id, score))` on a match, or `Err(NoMatch)` when no stored template
    /// is similar enough.
    ///
    /// During duplicate detection at enrollment time, call with `buf = 1`, `start_page = 0`,
    /// `page_count = <library capacity>`.
    pub fn search(
        &mut self,
        buf: u8,
        start_page: u16,
        page_count: u16,
    ) -> Result<(u16, u16), FingerprintError<E>> {
        let [sp_hi, sp_lo] = start_page.to_be_bytes();
        let [pc_hi, pc_lo] = page_count.to_be_bytes();
        self.send_command(&[PS_SEARCH, buf, sp_hi, sp_lo, pc_hi, pc_lo])?;
        let frame = self.read_ack()?; // returns Err(NoMatch) on confirm 0x09
        if frame.data.len() < 5 {
            return Err(FingerprintError::BadFrame);
        }
        let page_id = u16::from_be_bytes([frame.data[1], frame.data[2]]);
        let score = u16::from_be_bytes([frame.data[3], frame.data[4]]);
        Ok((page_id, score))
    }

    /// Merge CharBuffer 1 and CharBuffer 2 into a single template (`PS_REG_MODEL`, 0x05).
    ///
    /// Both buffers must be populated (via [`gen_char`](Self::gen_char)) before calling this.
    /// The resulting template is written back into CharBuffer 1 and is ready to be
    /// permanently stored with [`store_char`](Self::store_char).
    pub fn reg_model(&mut self) -> Result<(), FingerprintError<E>> {
        self.send_command(&[PS_REG_MODEL])?;
        self.read_ack()?;
        Ok(())
    }

    /// Persist a character buffer to the sensor's flash library (`PS_STORE_CHAR`, 0x06).
    ///
    /// Writes the template from CharBuffer `buf` (typically 1, after [`reg_model`](Self::reg_model))
    /// into flash at `page_id`. This is the final step of a manual enrollment sequence.
    ///
    /// Uses [`READ_TIMEOUT_FLASH_MS`] for the ACK deadline: writing to the sensor's
    /// internal flash can take over 500 ms, well beyond the standard command timeout.
    pub fn store_char(&mut self, buf: u8, page_id: u16) -> Result<(), FingerprintError<E>> {
        let [pid_hi, pid_lo] = page_id.to_be_bytes();
        self.send_command(&[PS_STORE_CHAR, buf, pid_hi, pid_lo])?;
        self.read_ack_timeout(READ_TIMEOUT_FLASH_MS)?;
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

    /// Erase the entire template library on the sensor (`PS_Empty`).
    pub fn empty_template_library(&mut self) -> Result<(), FingerprintError<E>> {
        self.send_command(&[crate::commands::PS_EMPTY])?;
        self.read_ack()?;
        Ok(())
    }

    /// Set the operating mode of the sensor.
    ///
    /// 0: Timed Sleep Mode
    /// 1: Active Mode
    pub fn set_work_mode(&mut self, mode: u8) -> Result<(), FingerprintError<E>> {
        self.send_command(&[crate::commands::PS_SET_WORK_MODE, mode])?;
        self.read_ack()?;
        Ok(())
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

    // =========================================================================
    // NoopDelay — zero-cost delay for unit tests (no thread::sleep)
    // =========================================================================

    struct NoopDelay;

    impl embedded_hal::delay::DelayNs for NoopDelay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    use super::*;
    use crate::commands::{PS_DELET_CHAR, PS_HANDSHAKE};
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
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x00])), NoopDelay);

        assert_eq!(driver.handshake(), Ok(()));
        assert_eq!(extract_tx_data(&driver.uart.tx), vec![PS_HANDSHAKE]);
    }

    /// Empty rx → retries exhausted → Timeout.
    #[test]
    fn handshake_no_response() {
        let mut driver = Fingerprint2Driver::new(MockUart::new(), NoopDelay);
        assert_eq!(driver.handshake(), Err(FingerprintError::Timeout));
    }

    // =========================================================================
    // auto_identify
    // =========================================================================

    /// confirm code 0x09 on any ACK → NoMatch (error propagated from read_ack).
    #[test]
    fn auto_identify_no_match() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x09])), NoopDelay);
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
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(rx), NoopDelay);

        assert_eq!(driver.auto_identify(3), Ok((5, 250)));
    }

    /// Sensor sends only the VERIFY frame (no intermediate ACKs) — still works.
    #[test]
    fn auto_identify_success_no_intermediate() {
        let rx = ack_bytes(&[0x00, 0x05, 0x00, 0x05, 0x00, 0xFA]);
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(rx), NoopDelay);
        assert_eq!(driver.auto_identify(3), Ok((5, 250)));
    }

    // =========================================================================
    // delete_template
    // =========================================================================

    /// Verifies PS_DELET_CHAR opcode and big-endian page_id / count bytes.
    #[test]
    fn delete_template_encoding() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x00])), NoopDelay);

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
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x00])), NoopDelay);
        assert_eq!(driver.activate(), Ok(()));
        assert_eq!(extract_tx_data(&driver.uart.tx), vec![PS_ACTIVATE]);
    }

    /// Empty rx → Timeout.
    #[test]
    fn activate_no_response() {
        let mut driver = Fingerprint2Driver::new(MockUart::new(), NoopDelay);
        assert_eq!(driver.activate(), Err(FingerprintError::Timeout));
    }

    // =========================================================================
    // Confirm code mapping
    // =========================================================================

    /// Unmapped confirm code (0x15 = wrong password) → SensorError(0x15).
    #[test]
    fn unmapped_sensor_error_propagated() {
        let mut driver = Fingerprint2Driver::new(MockUart::with_rx(ack_bytes(&[0x15])), NoopDelay);
        assert_eq!(driver.handshake(), Err(FingerprintError::SensorError(0x15)));
    }
}
