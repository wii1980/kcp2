mod common;
mod types;

#[cfg(feature = "alloc")]
mod alloc_impl;
#[cfg(all(not(feature = "alloc"), feature = "heapless"))]
mod heapless_impl;

pub use types::{KcpOutput, LinkState, SendHandle};

#[cfg(feature = "alloc")]
pub use alloc_impl::Kcp;
#[cfg(all(not(feature = "alloc"), feature = "heapless"))]
pub use heapless_impl::Kcp;
