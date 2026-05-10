use crate::errors::{KcpError, Result};

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(all(not(feature = "alloc"), feature = "heapless"))]
use heapless::Vec as HeaplessVec;

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

        let len = u32::from_le_bytes(data[20..24].try_into().unwrap()) as usize;
        let total = 24 + len;

        if data.len() < total {
            return Err(KcpError::InputTooShort {
                len: data.len(),
                min: total,
            });
        }

        let seg = Self {
            conv: u32::from_le_bytes(data[0..4].try_into().unwrap()),
            cmd: data[4],
            frg: data[5],
            wnd: u16::from_le_bytes(data[6..8].try_into().unwrap()),
            ts: u32::from_le_bytes(data[8..12].try_into().unwrap()),
            sn: u32::from_le_bytes(data[12..16].try_into().unwrap()),
            una: u32::from_le_bytes(data[16..20].try_into().unwrap()),
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
            ..Self::new()
        };

        Ok((seg, total))
    }

    #[cfg(feature = "std")]
    pub fn encode(&self, buf: &mut impl std::io::Write) -> std::io::Result<usize> {
        let mut header = [0u8; 24];
        header[0..4].copy_from_slice(&self.conv.to_le_bytes());
        header[4] = self.cmd;
        header[5] = self.frg;
        header[6..8].copy_from_slice(&self.wnd.to_le_bytes());
        header[8..12].copy_from_slice(&self.ts.to_le_bytes());
        header[12..16].copy_from_slice(&self.sn.to_le_bytes());
        header[16..20].copy_from_slice(&self.una.to_le_bytes());
        header[20..24].copy_from_slice(&(self.data.len() as u32).to_le_bytes());

        buf.write_all(&header)?;
        buf.write_all(&self.data)?;
        Ok(24 + self.data.len())
    }

    #[cfg(feature = "std")]
    pub fn decode(buf: &mut impl std::io::Read) -> std::io::Result<Self> {
        let mut seg = Self::new();
        let mut header = [0u8; 24];

        buf.read_exact(&mut header)?;

        seg.conv = u32::from_le_bytes(header[0..4].try_into().unwrap());
        seg.cmd = header[4];
        seg.frg = header[5];
        seg.wnd = u16::from_le_bytes(header[6..8].try_into().unwrap());
        seg.ts = u32::from_le_bytes(header[8..12].try_into().unwrap());
        seg.sn = u32::from_le_bytes(header[12..16].try_into().unwrap());
        seg.una = u32::from_le_bytes(header[16..20].try_into().unwrap());
        let len = u32::from_le_bytes(header[20..24].try_into().unwrap()) as usize;

        let mut data_vec = vec![0u8; len];
        buf.read_exact(&mut data_vec)?;
        seg.data = data_vec;
        Ok(seg)
    }
}
