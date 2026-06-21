use crate::errors::{KcpError, Result};

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(all(not(feature = "alloc"), feature = "heapless"))]
use heapless::Vec as HeaplessVec;

#[inline]
fn u32_from_le(buf: &[u8]) -> u32 {
    let mut arr = [0u8; 4];
    arr.copy_from_slice(&buf[..4]);
    u32::from_le_bytes(arr)
}

#[inline]
fn u16_from_le(buf: &[u8]) -> u16 {
    let mut arr = [0u8; 2];
    arr.copy_from_slice(&buf[..2]);
    u16::from_le_bytes(arr)
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub conv: u32,
    pub cmd: u8,
    pub frg: u8,
    pub wnd: u16,
    pub ts: u32,
    pub sn: u32,
    pub una: u32,
    pub resendts: u32,
    pub rto: u32,
    pub fastack: u32,
    pub xmit: u32,
    #[cfg(feature = "alloc")]
    pub data: Vec<u8>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    pub data: HeaplessVec<u8, 1400>,
    #[cfg(not(any(feature = "alloc", feature = "heapless")))]
    pub data_len: usize,
}

impl Default for Segment {
    fn default() -> Self {
        Self::new()
    }
}

impl Segment {
    pub fn new() -> Self {
        Self {
            conv: 0,
            cmd: 0,
            frg: 0,
            wnd: 0,
            ts: 0,
            sn: 0,
            una: 0,
            resendts: 0,
            rto: 0,
            fastack: 0,
            xmit: 0,
            #[cfg(feature = "alloc")]
            data: Vec::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            data: HeaplessVec::new(),
            #[cfg(not(any(feature = "alloc", feature = "heapless")))]
            data_len: 0,
        }
    }

    pub fn encode_to_slice(&self, buf: &mut [u8]) -> Result<usize> {
        #[cfg(any(feature = "alloc", feature = "heapless"))]
        let data_len = self.data.len();
        #[cfg(not(any(feature = "alloc", feature = "heapless")))]
        let data_len = self.data_len;

        let total = 24 + data_len;
        if buf.len() < total {
            return Err(KcpError::BufferTooSmall {
                required: total,
                available: buf.len(),
            });
        }

        buf[0..4].copy_from_slice(&self.conv.to_le_bytes());
        buf[4] = self.cmd;
        buf[5] = self.frg;
        buf[6..8].copy_from_slice(&self.wnd.to_le_bytes());
        buf[8..12].copy_from_slice(&self.ts.to_le_bytes());
        buf[12..16].copy_from_slice(&self.sn.to_le_bytes());
        buf[16..20].copy_from_slice(&self.una.to_le_bytes());
        buf[20..24].copy_from_slice(&(data_len as u32).to_le_bytes());

        #[cfg(any(feature = "alloc", feature = "heapless"))]
        buf[24..total].copy_from_slice(&self.data);

        Ok(total)
    }

    pub fn decode_from_slice(data: &[u8]) -> Result<(Self, usize)> {
        if data.len() < 24 {
            return Err(KcpError::InputTooShort {
                len: data.len(),
                min: 24,
            });
        }

        let len = u32_from_le(&data[20..24]) as usize;
        let total = 24 + len;

        if data.len() < total {
            return Err(KcpError::InputTooShort {
                len: data.len(),
                min: total,
            });
        }

        let seg = Self {
            conv: u32_from_le(&data[0..4]),
            cmd: data[4],
            frg: data[5],
            wnd: u16_from_le(&data[6..8]),
            ts: u32_from_le(&data[8..12]),
            sn: u32_from_le(&data[12..16]),
            una: u32_from_le(&data[16..20]),
            resendts: 0,
            rto: 0,
            fastack: 0,
            xmit: 0,
            #[cfg(feature = "alloc")]
            data: Vec::from(&data[24..total]),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            data: HeaplessVec::from_slice(&data[24..total]).map_err(|_| {
                KcpError::BufferTooSmall {
                    required: len,
                    available: 1400,
                }
            })?,
            #[cfg(not(any(feature = "alloc", feature = "heapless")))]
            data_len: len,
        };

        Ok((seg, total))
    }
}

#[cfg(test)]
#[cfg(feature = "alloc")]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_segment_encode_decode_roundtrip() {
        let mut seg = Segment::new();
        seg.conv = 0x1122_3344;
        seg.cmd = 81;
        seg.frg = 0;
        seg.wnd = 128;
        seg.ts = 1000;
        seg.sn = 1;
        seg.una = 0;
        seg.data = vec![1, 2, 3, 4, 5];

        let mut buf = [0u8; 256];
        let written = seg.encode_to_slice(&mut buf).unwrap();

        let (decoded, consumed) = Segment::decode_from_slice(&buf[..written]).unwrap();
        assert_eq!(consumed, written);
        assert_eq!(seg.conv, decoded.conv);
        assert_eq!(seg.cmd, decoded.cmd);
        assert_eq!(seg.frg, decoded.frg);
        assert_eq!(seg.wnd, decoded.wnd);
        assert_eq!(seg.ts, decoded.ts);
        assert_eq!(seg.sn, decoded.sn);
        assert_eq!(seg.una, decoded.una);
        assert_eq!(seg.data, decoded.data);
    }

    #[test]
    fn test_segment_decode_truncated() {
        let mut seg = Segment::new();
        seg.conv = 0x1122_3344;
        seg.cmd = 81;
        seg.data = vec![1, 2, 3, 4, 5];

        let mut buf = [0u8; 256];
        let written = seg.encode_to_slice(&mut buf).unwrap();

        let result = Segment::decode_from_slice(&buf[..written - 2]);
        assert!(result.is_err());
    }

    #[test]
    fn test_segment_decode_header_too_short() {
        let short = [0u8; 10];
        let result = Segment::decode_from_slice(&short);
        assert!(result.is_err());
    }

    #[test]
    fn test_segment_encode_buffer_too_small() {
        let mut seg = Segment::new();
        seg.conv = 0x1122_3344;
        seg.cmd = 81;
        seg.data = vec![0u8; 100];

        let mut small_buf = [0u8; 20];
        let result = seg.encode_to_slice(&mut small_buf);
        assert!(matches!(result, Err(KcpError::BufferTooSmall { .. })));
    }

    #[test]
    fn test_segment_default() {
        let seg = Segment::new();
        assert_eq!(seg.conv, 0);
        assert_eq!(seg.cmd, 0);
        assert_eq!(seg.frg, 0);
        assert_eq!(seg.wnd, 0);
        assert!(seg.data.is_empty());
    }

    #[test]
    fn test_segment_zero_length_data() {
        let mut seg = Segment::new();
        seg.conv = 42;
        seg.cmd = 82;
        seg.data = vec![];

        let mut buf = [0u8; 256];
        let written = seg.encode_to_slice(&mut buf).unwrap();
        assert_eq!(written, 24);

        let (decoded, _) = Segment::decode_from_slice(&buf[..written]).unwrap();
        assert_eq!(decoded.conv, 42);
        assert_eq!(decoded.cmd, 82);
        assert!(decoded.data.is_empty());
    }
}
