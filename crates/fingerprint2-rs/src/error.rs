//! Error types for the `fingerprint2-rs` UART driver.

/// All error conditions that the driver can surface to the caller.
///
/// The type parameter `E` is the underlying UART error type (e.g.
/// `core::convert::Infallible` in tests, or the real HAL error in firmware).
#[derive(Debug, PartialEq)]
pub enum FingerprintError<E> {
    /// Received frame begins with wrong magic bytes (expected `0xEF01`),
    /// or the frame is too short / the `LEN` field does not match the actual
    /// number of bytes present in the buffer.
    BadFrame,

    /// Received frame has a checksum mismatch.
    BadChecksum,

    /// No bytes arrived within the polling window.
    ///
    /// In tests: `MockUart` / `LimitedMockUart` rx buffer exhausted before a
    /// full frame was assembled.
    Timeout,

    /// `PS_AutoIdentify` returned a "no match" confirmation code (`0x09`).
    NoMatch,

    /// `PS_AutoEnroll` failed (poor image quality, repeated low-area reads,
    /// merge failure, etc.).
    ///
    /// Triggered by confirmation codes `0x03`, `0x06`, `0x07`, or `0x0A`.
    EnrollFailed,

    /// Sensor returned a non-zero confirmation code not otherwise mapped.
    ///
    /// The raw confirmation byte is preserved for diagnostics.
    SensorError(u8),

    /// Underlying UART read or write error.
    Uart(E),
}
