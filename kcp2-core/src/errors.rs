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
    SendBackpressure {
        wait_snd: usize,
        max: usize,
    },
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
            Self::SendBackpressure { wait_snd, max } => {
                write!(f, "send backpressure: wait_snd {wait_snd} >= max {max}")
            }
            Self::IoError(msg) => write!(f, "IO error: {msg}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for KcpError {}

pub type Result<T> = core::result::Result<T, KcpError>;

#[cfg(test)]
#[cfg(feature = "alloc")]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn test_error_display_conv_mismatch() {
        let err = KcpError::ConvMismatch {
            expected: 42,
            got: 99,
        };
        let msg = format!("{err}");
        assert!(msg.contains("42"));
        assert!(msg.contains("99"));
    }

    #[test]
    fn test_error_display_all_variants() {
        let _ = format!("{}", KcpError::EmptyData);
        let _ = format!("{}", KcpError::RecvQueueEmpty);
        let _ = format!("{}", KcpError::IncompletePacket);
        let _ = format!("{}", KcpError::DeadLink);
        let _ = format!("{}", KcpError::Timeout);
        let _ = format!("{}", KcpError::TooManyFragments {
            count: 10,
            max: 5,
        });
        let _ = format!("{}", KcpError::MtuTooSmall {
            mtu: 50,
            min: 100,
        });
        let _ = format!("{}", KcpError::InputTooShort {
            len: 5,
            min: 24,
        });
        let _ = format!("{}", KcpError::BufferTooSmall {
            required: 100,
            available: 50,
        });
        let _ = format!("{}", KcpError::SendBackpressure {
            wait_snd: 5,
            max: 3,
        });
        let _ = format!("{}", KcpError::InvalidCmd { cmd: 99 });
        let _ = format!("{}", KcpError::IoError(alloc::string::String::from("test error")));
    }

    #[test]
    fn test_error_clone() {
        let err = KcpError::DeadLink;
        let err2 = err.clone();
        assert_eq!(format!("{err}"), format!("{err2}"));
    }
}
