use core::fmt;

#[derive(Debug, Clone)]
pub enum KcpError {
    ConvMismatch {
        expected: u32,
        got: u32,
    },
    InvalidCmd {
        cmd: u8,
    },
    EmptyData,
    TooManyFragments {
        count: usize,
        max: usize,
    },
    MtuTooSmall {
        mtu: usize,
        min: usize,
    },
    InputTooShort {
        len: usize,
        min: usize,
    },
    RecvQueueEmpty,
    IncompletePacket,
    BufferTooSmall {
        required: usize,
        available: usize,
    },
    DeadLink,
    Timeout,
    #[cfg(feature = "alloc")]
    IoError(alloc::string::String),
    #[cfg(not(feature = "alloc"))]
    IoError(&'static str),
}

impl fmt::Display for KcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConvMismatch { expected, got } => {
                write!(
                    f,
                    "conversation id mismatch: expected {expected}, got {got}"
                )
            }
            Self::InvalidCmd { cmd } => write!(f, "invalid command: {cmd}"),
            Self::EmptyData => write!(f, "empty data"),
            Self::TooManyFragments { count, max } => {
                write!(f, "too many fragments: {count} >= {max}")
            }
            Self::MtuTooSmall { mtu, min } => write!(f, "MTU too small: {mtu} < {min}"),
            Self::InputTooShort { len, min } => {
                write!(f, "input too short: {len} < {min}")
            }
            Self::RecvQueueEmpty => write!(f, "receive queue empty"),
            Self::IncompletePacket => write!(f, "incomplete packet"),
            Self::BufferTooSmall {
                required,
                available,
            } => {
                write!(
                    f,
                    "buffer too small: required {required}, available {available}"
                )
            }
            Self::DeadLink => write!(f, "dead link"),
            Self::Timeout => write!(f, "operation timed out"),
            Self::IoError(msg) => write!(f, "IO error: {msg}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for KcpError {}

pub type Result<T> = core::result::Result<T, KcpError>;
