//! io_uring based high-performance I/O module

#[cfg(feature = "io_uring")]
pub mod uring;

#[cfg(feature = "io_uring")]
pub mod buffer;

#[cfg(feature = "io_uring")]
pub use uring::{IoUringExecutor, IoUringConfig, IoUringMetrics};

#[cfg(feature = "io_uring")]
pub use buffer::{BufferPool, AlignedBuffer};

/// Check if io_uring is available on the system
pub fn check_io_uring_support() -> bool {
    #[cfg(feature = "io_uring")]
    {
        // Try to create a small io_uring instance to check support
        match io_uring::IoUring::new(2) {
            Ok(_) => true,
            Err(_) => false,
        }
    }
    
    #[cfg(not(feature = "io_uring"))]
    {
        false
    }
}

/// Get io_uring kernel version requirements
pub fn get_io_uring_requirements() -> &'static str {
    "Linux 5.10+ (basic support), 5.11+ (splice), 5.19+ (fixed buffers)"
}