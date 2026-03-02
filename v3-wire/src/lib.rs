//! v3-wire
//!
//! Compact wire framing for V3 datagrams.

use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const WIRE_VERSION_V3: u8 = 0x03;
pub const HEADER_LEN: usize = 1 + 1 + 1 + 8 + 2 + 2;
pub const MAX_TLV_COUNT: usize = 64;
pub const MAX_TLV_VALUE_LEN: usize = 2048;
pub const MAX_PAYLOAD_LEN: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MessageType {
    ClientHello = 0x01,
    ServerHello = 0x02,
    HelloAck = 0x03,
    Msr = 0x10,
    RequestRange = 0x11,
    Data = 0x12,
    Fragment = 0x13,
    Error = 0x7e,
    Close = 0x7f,
}

impl MessageType {
    fn from_u8(value: u8) -> Result<Self, WireError> {
        Ok(match value {
            0x01 => Self::ClientHello,
            0x02 => Self::ServerHello,
            0x03 => Self::HelloAck,
            0x10 => Self::Msr,
            0x11 => Self::RequestRange,
            0x12 => Self::Data,
            0x13 => Self::Fragment,
            0x7e => Self::Error,
            0x7f => Self::Close,
            _ => return Err(WireError::UnknownMessageType(value)),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tlv {
    pub kind: u8,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFrame {
    pub version: u8,
    pub message_type: MessageType,
    pub flags: u8,
    pub cid: u64,
    pub payload: Vec<u8>,
    pub tlvs: Vec<Tlv>,
}

impl WireFrame {
    pub fn new(
        message_type: MessageType,
        flags: u8,
        cid: u64,
        payload: Vec<u8>,
        tlvs: Vec<Tlv>,
    ) -> Self {
        Self { version: WIRE_VERSION_V3, message_type, flags, cid, payload, tlvs }
    }

    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        validate_frame(self)?;

        let payload_len = self.payload.len() as u16;
        let mut tlv_block = BytesMut::new();
        for tlv in &self.tlvs {
            if tlv.value.len() > MAX_TLV_VALUE_LEN {
                return Err(WireError::TlvTooLarge(tlv.value.len()));
            }
            tlv_block.put_u8(tlv.kind);
            tlv_block.put_u16(tlv.value.len() as u16);
            tlv_block.extend_from_slice(&tlv.value);
        }
        let tlv_len = tlv_block.len();
        if tlv_len > u16::MAX as usize {
            return Err(WireError::TlvBlockTooLarge(tlv_len));
        }

        let mut out = BytesMut::with_capacity(HEADER_LEN + self.payload.len() + tlv_len);
        out.put_u8(self.version);
        out.put_u8(self.message_type as u8);
        out.put_u8(self.flags);
        out.put_u64(self.cid);
        out.put_u16(payload_len);
        out.put_u16(tlv_len as u16);
        out.extend_from_slice(&self.payload);
        out.extend_from_slice(&tlv_block);
        Ok(out.to_vec())
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        if input.len() < HEADER_LEN {
            return Err(WireError::Truncated);
        }

        let mut buf = input;
        let version = buf.get_u8();
        if version != WIRE_VERSION_V3 {
            return Err(WireError::UnsupportedVersion(version));
        }

        let message_type = MessageType::from_u8(buf.get_u8())?;
        let flags = buf.get_u8();
        let cid = buf.get_u64();
        let payload_len = buf.get_u16() as usize;
        let tlv_len = buf.get_u16() as usize;

        if payload_len > MAX_PAYLOAD_LEN {
            return Err(WireError::PayloadTooLarge(payload_len));
        }

        if buf.remaining() != payload_len + tlv_len {
            return Err(WireError::LengthMismatch {
                remaining: buf.remaining(),
                expected: payload_len + tlv_len,
            });
        }

        let mut payload = vec![0u8; payload_len];
        buf.copy_to_slice(&mut payload);

        let tlv_bytes = &buf[..tlv_len];
        let tlvs = decode_tlvs(tlv_bytes)?;

        Ok(Self { version, message_type, flags, cid, payload, tlvs })
    }
}

fn decode_tlvs(mut input: &[u8]) -> Result<Vec<Tlv>, WireError> {
    let mut out = Vec::new();
    while !input.is_empty() {
        if input.len() < 3 {
            return Err(WireError::MalformedTlv);
        }
        if out.len() >= MAX_TLV_COUNT {
            return Err(WireError::TooManyTlvs(out.len()));
        }
        let kind = input.get_u8();
        let len = input.get_u16() as usize;
        if len > MAX_TLV_VALUE_LEN {
            return Err(WireError::TlvTooLarge(len));
        }
        if input.remaining() < len {
            return Err(WireError::MalformedTlv);
        }
        let mut value = vec![0u8; len];
        input.copy_to_slice(&mut value);
        out.push(Tlv { kind, value });
    }
    Ok(out)
}

fn validate_frame(frame: &WireFrame) -> Result<(), WireError> {
    if frame.version != WIRE_VERSION_V3 {
        return Err(WireError::UnsupportedVersion(frame.version));
    }
    if frame.payload.len() > MAX_PAYLOAD_LEN {
        return Err(WireError::PayloadTooLarge(frame.payload.len()));
    }
    if frame.tlvs.len() > MAX_TLV_COUNT {
        return Err(WireError::TooManyTlvs(frame.tlvs.len()));
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WireError {
    #[error("input is truncated")]
    Truncated,
    #[error("unsupported version: {0:#x}")]
    UnsupportedVersion(u8),
    #[error("unknown message type: {0:#x}")]
    UnknownMessageType(u8),
    #[error("payload too large: {0}")]
    PayloadTooLarge(usize),
    #[error("too many TLVs: {0}")]
    TooManyTlvs(usize),
    #[error("TLV too large: {0}")]
    TlvTooLarge(usize),
    #[error("TLV block too large: {0}")]
    TlvBlockTooLarge(usize),
    #[error("malformed TLV")]
    MalformedTlv,
    #[error("length mismatch (remaining={remaining}, expected={expected})")]
    LengthMismatch { remaining: usize, expected: usize },
}

/// Parser fuzz entrypoint.
pub fn fuzz_decode(input: &[u8]) {
    let _ = WireFrame::decode(input);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_msr_frame() {
        let frame = WireFrame::new(
            MessageType::Msr,
            0b0000_0011,
            0x0102_0304_0506_0708,
            vec![1, 2, 3, 4],
            vec![Tlv { kind: 1, value: vec![0x10, 0x11] }, Tlv { kind: 2, value: vec![0x20] }],
        );

        let encoded = frame.encode().expect("encode");
        let decoded = WireFrame::decode(&encoded).expect("decode");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn golden_vector_client_hello() {
        let frame = WireFrame::new(
            MessageType::ClientHello,
            0x00,
            0x1111_2222_3333_4444,
            vec![0xaa, 0xbb],
            vec![Tlv { kind: 0x09, value: vec![0x01, 0x02, 0x03] }],
        );
        let got = frame.encode().expect("encode");

        // version=03, type=01, flags=00, cid=1111222233334444,
        // payload_len=0002, tlv_len=0006, payload=aabb, tlv=09 0003 010203
        let expected_hex = "030100111122223333444400020006aabb090003010203";
        assert_eq!(hex_encode(&got), expected_hex);
    }

    #[test]
    fn negotiation_vector_server_hello_with_capabilities() {
        let frame = WireFrame::new(
            MessageType::ServerHello,
            0x01,
            0xabab_cdcd_0102_0304,
            vec![0x01], // security mode selected
            vec![
                // capabilities bitmap
                Tlv { kind: 0x30, value: vec![0b0000_0111] },
                // timer negotiation hint blob (opaque at wire layer)
                Tlv {
                    kind: 0x31,
                    value: vec![0x00, 0x00, 0x3a, 0x98], // 15000 ms
                },
            ],
        );

        let encoded = frame.encode().expect("encode");
        let decoded = WireFrame::decode(&encoded).expect("decode");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn negotiation_vector_unknown_tlv_is_preserved() {
        let frame = WireFrame::new(
            MessageType::HelloAck,
            0,
            1,
            vec![],
            vec![Tlv { kind: 0xfe, value: vec![0xde, 0xad, 0xbe, 0xef] }],
        );
        let encoded = frame.encode().expect("encode");
        let decoded = WireFrame::decode(&encoded).expect("decode");
        assert_eq!(decoded.tlvs.len(), 1);
        assert_eq!(decoded.tlvs[0].kind, 0xfe);
        assert_eq!(decoded.tlvs[0].value, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn decode_rejects_invalid_version() {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes[0] = 0x01;
        assert_eq!(WireFrame::decode(&bytes), Err(WireError::UnsupportedVersion(0x01)));
    }

    #[test]
    fn decode_rejects_truncated_tlv() {
        let frame = WireFrame::new(
            MessageType::Data,
            0,
            7,
            vec![1],
            vec![Tlv { kind: 1, value: vec![2, 3, 4] }],
        );
        let mut bytes = frame.encode().expect("encode");
        bytes.pop();
        assert!(matches!(WireFrame::decode(&bytes), Err(WireError::LengthMismatch { .. })));
    }

    #[test]
    fn decode_rejects_unknown_message_type() {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes[0] = WIRE_VERSION_V3;
        bytes[1] = 0xfe;
        assert_eq!(WireFrame::decode(&bytes), Err(WireError::UnknownMessageType(0xfe)));
    }

    fn hex_encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(hex_char((b >> 4) & 0x0f));
            out.push(hex_char(b & 0x0f));
        }
        out
    }

    fn hex_char(v: u8) -> char {
        match v {
            0..=9 => (b'0' + v) as char,
            10..=15 => (b'a' + (v - 10)) as char,
            _ => '?',
        }
    }
}
