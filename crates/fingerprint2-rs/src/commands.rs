//! Opcode constants and typed parameter types for the Fingerprint2 protocol.
//!
//! Opcode names are preserved verbatim from the M5Stack STM32 firmware source
//! so that `grep PS_AutoEnroll` maps directly between the C++ reference and
//! this Rust driver.

// ---------------------------------------------------------------------------
// Opcode constants
// ---------------------------------------------------------------------------

/// Collect a finger image into the image buffer (identification / smart-poll use).
pub const PS_GET_IMAGE: u8 = 0x01;
/// Collect a finger image optimised for enrollment quality.
///
/// Functionally identical to [`PS_GET_IMAGE`] at the protocol level (no parameters,
/// same ACK format), but the sensor firmware may apply different internal thresholds.
/// Always prefer this opcode during enrollment passes; use [`PS_GET_IMAGE`] for
/// identification polling.
pub const PS_GET_ENROLL_IMAGE: u8 = 0x29;
/// Generate a character file from the image buffer into a char buffer slot.
pub const PS_GEN_CHAR: u8 = 0x02;
/// Search the library for a matching template.
pub const PS_SEARCH: u8 = 0x04;
/// Combine two char buffers into a template.
pub const PS_REG_MODEL: u8 = 0x05;
/// Store a template from a char buffer into flash at a given page ID.
pub const PS_STORE_CHAR: u8 = 0x06;
/// Delete one or more stored templates.
///
/// Note: the upstream typo (`DeletChar`, not `DeleteChar`) is preserved for
/// traceability with the M5Stack C++ source.
pub const PS_DELET_CHAR: u8 = 0x0C;
/// Empty the entire template library.
pub const PS_EMPTY: u8 = 0x0D;
/// High-level autonomous identification — returns the matched page ID.
pub const PS_AUTO_IDENTIFY: u8 = 0x32;
/// Verify that the module is powered on and responsive.
pub const PS_HANDSHAKE: u8 = 0x35;
/// Set the operating mode of the sensor.
pub const PS_SET_WORK_MODE: u8 = 0xD2;
/// Activate the sensor (required after cold boot on some firmware versions).
pub const PS_ACTIVATE: u8 = 0xD4;
