//! Frame serialization and deserialization for the Fingerprint2 UART protocol.
//!
//! ## Wire format
//!
//! ```text
//! Offset  Size  Field
//!   0      2    Magic  — always 0xEF 0x01
//!   2      4    Addr   — device address (default 0xFFFF_FFFF = broadcast)
//!   6      1    Type   — PacketType discriminant
//!   7      2    LEN    — len(DATA) + 2  (the +2 accounts for the checksum)
//!   9      N    DATA   — payload bytes
//!   9+N    2    CSUM   — (Type + LEN + Σ DATA) & 0xFFFF, big-endian
//! ```
//!
//! Minimum valid frame (zero DATA bytes): **11 bytes**.

use heapless::Vec;

use crate::error::FingerprintError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// First two bytes of every valid frame.
pub const FRAME_MAGIC: u16 = 0xEF01;

/// Default device address used when no specific address is assigned.
pub const DEFAULT_ADDR: u32 = 0xFFFF_FFFF;

/// Maximum number of payload bytes in a single frame.
pub const MAX_DATA_LEN: usize = 64;

/// Minimum number of bytes in a valid frame (no DATA, just header + checksum).
/// 2 (magic) + 4 (addr) + 1 (type) + 2 (len) + 0 (data) + 2 (csum) = 11
pub const MIN_FRAME_LEN: usize = 11;

// ---------------------------------------------------------------------------
// PacketType
// ---------------------------------------------------------------------------

/// Discriminates the role of a frame in the protocol exchange.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PacketType {
    /// A command sent from the host to the sensor.
    Command = 0x01,
    /// A data continuation frame (bulk transfers).
    Data = 0x02,
    /// An acknowledgement / response frame from the sensor.
    Ack = 0x07,
    /// Final frame of a multi-packet data transfer.
    EndOfData = 0x08,
}

impl TryFrom<u8> for PacketType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(PacketType::Command),
            0x02 => Ok(PacketType::Data),
            0x07 => Ok(PacketType::Ack),
            0x08 => Ok(PacketType::EndOfData),
            _ => Err(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// A single protocol frame exchanged with the sensor.
#[derive(Debug, PartialEq)]
pub struct Frame {
    /// Device address. Normally [`DEFAULT_ADDR`] (`0xFFFF_FFFF`).
    pub addr: u32,
    /// Whether this frame is a command, ack, data, etc.
    pub packet_type: PacketType,
    /// Payload bytes.
    ///
    /// For [`PacketType::Ack`] frames, `data[0]` is the sensor's confirmation
    /// code (`0x00` = success).
    pub data: Vec<u8, MAX_DATA_LEN>,
}

// ---------------------------------------------------------------------------
// Checksum helper
// ---------------------------------------------------------------------------

/// Compute the 16-bit checksum for a frame.
///
/// ```text
/// CSUM = (TYPE as u32 + LEN as u32 + Σ DATA[i] as u32) & 0xFFFF
/// ```
///
/// where `LEN = len(DATA) + 2`.
fn compute_checksum(packet_type: PacketType, data: &[u8]) -> u16 {
    let len = (data.len() + 2) as u32;
    let sum = packet_type as u32 + len + data.iter().map(|&b| b as u32).sum::<u32>();
    (sum & 0xFFFF) as u16
}

// ---------------------------------------------------------------------------
// Public codec API
// ---------------------------------------------------------------------------

/// Serialize a [`Frame`] into a caller-supplied byte buffer.
///
/// Returns the number of bytes written, or `None` if `buf` is too small.
pub fn serialize(frame: &Frame, buf: &mut [u8]) -> Option<usize> {
    let data_len = frame.data.len();
    // 2 (magic) + 4 (addr) + 1 (type) + 2 (len) + data_len + 2 (csum)
    let total = MIN_FRAME_LEN + data_len;

    if buf.len() < total {
        return None;
    }

    let len_field = (data_len + 2) as u16;
    let csum = compute_checksum(frame.packet_type, &frame.data);

    // Magic
    buf[0] = (FRAME_MAGIC >> 8) as u8;
    buf[1] = (FRAME_MAGIC & 0xFF) as u8;
    // Address (big-endian)
    buf[2..6].copy_from_slice(&frame.addr.to_be_bytes());
    // Type
    buf[6] = frame.packet_type as u8;
    // LEN (big-endian)
    buf[7] = (len_field >> 8) as u8;
    buf[8] = (len_field & 0xFF) as u8;
    // DATA
    buf[9..9 + data_len].copy_from_slice(&frame.data);
    // Checksum (big-endian)
    buf[9 + data_len] = (csum >> 8) as u8;
    buf[9 + data_len + 1] = (csum & 0xFF) as u8;

    Some(total)
}

/// Deserialize a [`Frame`] from a raw byte slice.
///
/// # Errors
///
/// - [`FingerprintError::BadFrame`] — wrong magic bytes, slice too short, or
///   the `LEN` field does not match the number of bytes present.
/// - [`FingerprintError::BadChecksum`] — the computed checksum does not match
///   the received checksum.
pub fn deserialize(buf: &[u8]) -> Result<Frame, FingerprintError<core::convert::Infallible>> {
    // Must have at least the minimum frame length
    if buf.len() < MIN_FRAME_LEN {
        return Err(FingerprintError::BadFrame);
    }

    // Magic
    if buf[0] != 0xEF || buf[1] != 0x01 {
        return Err(FingerprintError::BadFrame);
    }

    // Address
    let addr = u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]);

    // Packet type
    let packet_type = PacketType::try_from(buf[6]).map_err(|_| FingerprintError::BadFrame)?;

    // LEN field — must be >= 2 because it always counts the 2-byte checksum
    let len_field = u16::from_be_bytes([buf[7], buf[8]]) as usize;
    if len_field < 2 {
        return Err(FingerprintError::BadFrame);
    }

    let data_len = len_field - 2;

    // Guard against frames that claim absurdly large DATA sections
    if data_len > MAX_DATA_LEN {
        return Err(FingerprintError::BadFrame);
    }

    // The buffer must hold header (9) + data + checksum (2)
    let expected_total = 9 + data_len + 2;
    if buf.len() < expected_total {
        return Err(FingerprintError::BadFrame);
    }

    // Collect DATA bytes into a heapless Vec
    let mut data: Vec<u8, MAX_DATA_LEN> = Vec::new();
    for &b in &buf[9..9 + data_len] {
        // Capacity was checked above, so push cannot fail
        data.push(b).map_err(|_| FingerprintError::BadFrame)?;
    }

    // Validate checksum
    let received_csum = u16::from_be_bytes([buf[9 + data_len], buf[9 + data_len + 1]]);
    let expected_csum = compute_checksum(packet_type, &data);
    if received_csum != expected_csum {
        return Err(FingerprintError::BadChecksum);
    }

    Ok(Frame {
        addr,
        packet_type,
        data,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Build a Frame with DEFAULT_ADDR and arbitrary data bytes.
    fn make_frame(packet_type: PacketType, data: &[u8]) -> Frame {
        let mut d: Vec<u8, MAX_DATA_LEN> = Vec::new();
        for &b in data {
            d.push(b).unwrap();
        }
        Frame {
            addr: DEFAULT_ADDR,
            packet_type,
            data: d,
        }
    }

    // ------------------------------------------------------------------
    // Checksum round-trip
    // ------------------------------------------------------------------

    /// Serialize a known command frame, manually recompute the checksum, and
    /// assert it matches the last two bytes of the serialized output.
    ///
    /// For DATA = [PS_HANDSHAKE = 0x35]:
    ///   TYPE = 0x01, LEN = 0x0003, DATA = [0x35]
    ///   CSUM = (1 + 3 + 0x35) & 0xFFFF = 57 = 0x0039
    #[test]
    fn checksum_round_trip() {
        let frame = make_frame(PacketType::Command, &[0x35]);
        let mut buf = [0u8; 64];
        let n = serialize(&frame, &mut buf).unwrap();

        let received_csum = u16::from_be_bytes([buf[n - 2], buf[n - 1]]);
        // Manual recomputation: TYPE + LEN + DATA[0]
        let expected: u32 = (PacketType::Command as u32) + 3 + 0x35;
        assert_eq!(received_csum, (expected & 0xFFFF) as u16);
    }

    // ------------------------------------------------------------------
    // Frame round-trip
    // ------------------------------------------------------------------

    /// serialize → deserialize must reproduce every field without corruption.
    #[test]
    fn frame_round_trip() {
        let frame = make_frame(PacketType::Command, &[0x35]);
        let mut buf = [0u8; 64];
        let n = serialize(&frame, &mut buf).unwrap();

        let decoded = deserialize(&buf[..n]).unwrap();
        assert_eq!(decoded.addr, DEFAULT_ADDR);
        assert_eq!(decoded.packet_type, PacketType::Command);
        assert_eq!(decoded.data.as_slice(), &[0x35]);
    }

    // ------------------------------------------------------------------
    // Wrong magic bytes
    // ------------------------------------------------------------------

    /// Replacing the second magic byte must yield BadFrame.
    #[test]
    fn wrong_magic_returns_bad_frame() {
        let frame = make_frame(PacketType::Command, &[0x35]);
        let mut buf = [0u8; 64];
        let n = serialize(&frame, &mut buf).unwrap();

        buf[1] = 0x02; // corrupt EF *01* → EF 02
        assert_eq!(deserialize(&buf[..n]), Err(FingerprintError::BadFrame));
    }

    // ------------------------------------------------------------------
    // Checksum mismatch
    // ------------------------------------------------------------------

    /// Flipping a byte in the checksum must yield BadChecksum.
    #[test]
    fn checksum_mismatch_returns_bad_checksum() {
        let frame = make_frame(PacketType::Command, &[0x35]);
        let mut buf = [0u8; 64];
        let n = serialize(&frame, &mut buf).unwrap();

        buf[n - 1] ^= 0xFF; // flip all bits of the low checksum byte
        assert_eq!(deserialize(&buf[..n]), Err(FingerprintError::BadChecksum));
    }

    // ------------------------------------------------------------------
    // Truncated frame
    // ------------------------------------------------------------------

    /// A 7-byte slice (below the 11-byte minimum) must yield BadFrame.
    #[test]
    fn truncated_frame_returns_bad_frame() {
        let buf = [0xEF_u8, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x07];
        assert_eq!(deserialize(&buf), Err(FingerprintError::BadFrame));
    }

    // ------------------------------------------------------------------
    // LEN field mismatch
    // ------------------------------------------------------------------

    /// LEN claims 10 DATA bytes but the slice only contains 5 data bytes.
    /// deserialize must return BadFrame rather than reading out-of-bounds.
    #[test]
    fn len_mismatch_returns_bad_frame() {
        // Build a 16-byte buffer: header(9) + 5 data bytes + 2 csum bytes.
        // But set LEN = 12 (→ 10 data bytes expected).
        let mut buf = [0u8; 16];
        buf[0] = 0xEF;
        buf[1] = 0x01;
        buf[2..6].copy_from_slice(&DEFAULT_ADDR.to_be_bytes());
        buf[6] = PacketType::Command as u8;
        buf[7] = 0x00;
        buf[8] = 0x0C; // LEN = 12 → data_len = 10, but we only have 5 bytes

        // expected_total = 9 + 10 + 2 = 21, but buf.len() = 16 → BadFrame
        assert_eq!(deserialize(&buf), Err(FingerprintError::BadFrame));
    }

    // ------------------------------------------------------------------
    // Non-default address round-trip
    // ------------------------------------------------------------------

    /// A non-default address must survive the serialize → deserialize cycle
    /// without any bits being corrupted.
    #[test]
    fn non_default_addr_round_trips() {
        let mut data: Vec<u8, MAX_DATA_LEN> = Vec::new();
        data.push(0x01).unwrap();
        let frame = Frame {
            addr: 0xABCD_1234,
            packet_type: PacketType::Command,
            data,
        };

        let mut buf = [0u8; 64];
        let n = serialize(&frame, &mut buf).unwrap();
        let decoded = deserialize(&buf[..n]).unwrap();

        assert_eq!(decoded.addr, 0xABCD_1234);
    }
}
