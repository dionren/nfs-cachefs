use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use std::path::Path;
use std::os::unix::io::{AsRawFd, RawFd};
use std::io;

use io_uring::{IoUring, opcode, types, squeue, cqueue};
use parking_lot::Mutex;
use tracing::{info, debug, warn, error};

use crate::Result;
use crate::core::error::CacheFsError;
use super::buffer::{BufferPool, AlignedBuffer};

/// io_uring configuration
#[derive(Debug, Clone)]
pub struct IoUringConfig {
    /// Queue depth (number of entries)
    pub queue_depth: u32,
    /// Enable kernel submission queue polling
    pub sq_poll: bool,
    /// Enable kernel I/O polling
    pub io_poll: bool,
    /// Use fixed/registered buffers
    pub fixed_buffers: bool,
    /// Use huge pages for buffers
    pub huge_pages: bool,
    /// Idle time for SQ polling thread (ms)
    pub sq_poll_idle: u32,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            queue_depth: 256,
            sq_poll: false,
            io_poll: false,
            fixed_buffers: true,
            huge_pages: false,
            sq_poll_idle: 1000, // 1 second
        }
    }
}

/// io_uring performance metrics
#[derive(Debug, Default)]
pub struct IoUringMetrics {
    pub submissions: AtomicU64,
    pub completions: AtomicU64,
    pub sq_full_events: AtomicU64,
    pub cq_overflow_events: AtomicU64,
    pub total_bytes_read: AtomicU64,
    pub total_bytes_written: AtomicU64,
    pub avg_latency_us: AtomicU64,
    pub p99_latency_us: AtomicU64,
}

impl IoUringMetrics {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn record_submission(&self) {
        self.submissions.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_completion(&self, bytes: usize, latency_us: u64) {
        self.completions.fetch_add(1, Ordering::Relaxed);
        self.total_bytes_read.fetch_add(bytes as u64, Ordering::Relaxed);
        
        // Simple moving average for latency
        let old_avg = self.avg_latency_us.load(Ordering::Relaxed);
        let new_avg = (old_avg * 99 + latency_us) / 100;
        self.avg_latency_us.store(new_avg, Ordering::Relaxed);
        
        // Update P99 if this is in the top 1%
        let p99 = self.p99_latency_us.load(Ordering::Relaxed);
        if latency_us > p99 {
            self.p99_latency_us.store(latency_us, Ordering::Relaxed);
        }
    }
}

/// Main io_uring executor
pub struct IoUringExecutor {
    ring: Arc<Mutex<IoUring>>,
    config: IoUringConfig,
    buffer_pool: Arc<BufferPool>,
    metrics: Arc<IoUringMetrics>,
}

impl IoUringExecutor {
    /// Create a new io_uring executor
    pub fn new(config: IoUringConfig) -> Result<Self> {
        info!("Initializing io_uring executor with queue_depth={}", config.queue_depth);
        
        let mut builder = IoUring::builder();
        
        // Configure io_uring parameters
        if config.sq_poll {
            builder.setup_sqpoll(config.sq_poll_idle);
            info!("Enabled SQ polling with {} ms idle time", config.sq_poll_idle);
        }
        
        if config.io_poll {
            builder.setup_iopoll();
            info!("Enabled I/O polling mode");
        }
        
        // Create the io_uring instance
        let ring = builder
            .build(config.queue_depth)
            .map_err(|e| CacheFsError::io_error(format!("Failed to create io_uring: {}", e)))?;
            
        // Create buffer pool
        let buffer_pool_size = (config.queue_depth as usize).min(128);
        let buffer_pool = Arc::new(BufferPool::new(
            buffer_pool_size,
            4 * 1024 * 1024, // 4MB buffers
            config.huge_pages,
        )?);
        
        // Register buffers with io_uring if configured
        if config.fixed_buffers {
            // TODO: Implement buffer registration
            debug!("Fixed buffer registration enabled");
        }
        
        Ok(Self {
            ring: Arc::new(Mutex::new(ring)),
            config,
            buffer_pool,
            metrics: Arc::new(IoUringMetrics::new()),
        })
    }
    
    /// Read file data directly using io_uring
    pub async fn read_direct(
        &self,
        path: &Path,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>> {
        let start = Instant::now();
        
        // Open file
        let file = std::fs::File::open(path)
            .map_err(|e| CacheFsError::io_error(format!("Failed to open file: {}", e)))?;
        let fd = file.as_raw_fd();
        
        // Get buffer from pool
        let buffer = self.buffer_pool.acquire().await?;
        let read_size = size.min(buffer.size() as u32);
        
        // Create read operation
        let read_e = opcode::Read::new(types::Fd(fd), buffer.as_mut_ptr(), read_size)
            .offset(offset)
            .build()
            .user_data(0x42);
            
        // Submit to io_uring
        self.metrics.record_submission();
        
        let mut ring = self.ring.lock();
        unsafe {
            ring.submission()
                .push(&read_e)
                .map_err(|_| {
                    self.metrics.sq_full_events.fetch_add(1, Ordering::Relaxed);
                    CacheFsError::io_error("Submission queue full")
                })?;
        }
        
        // Submit and wait for completion
        ring.submit_and_wait(1)
            .map_err(|e| CacheFsError::io_error(format!("Submit failed: {}", e)))?;
            
        // Get completion
        let cqe = ring.completion().next()
            .ok_or_else(|| CacheFsError::io_error("No completion event"))?;
            
        let result = cqe.result();
        if result < 0 {
            return Err(CacheFsError::io_error(format!(
                "Read failed with error: {}",
                io::Error::from_raw_os_error(-result)
            )));
        }
        
        // Copy data from buffer
        let bytes_read = result as usize;
        let data = buffer.to_vec(bytes_read);
        
        // Record metrics
        let latency_us = start.elapsed().as_micros() as u64;
        self.metrics.record_completion(bytes_read, latency_us);
        
        debug!(
            "io_uring read: {} bytes from {} in {} μs",
            bytes_read, path.display(), latency_us
        );
        
        Ok(data)
    }
    
    /// Read using a fixed/registered buffer
    pub async fn read_fixed(
        &self,
        fd: RawFd,
        offset: u64,
        buf_index: u16,
        size: u32,
    ) -> Result<usize> {
        let start = Instant::now();
        
        // Create read operation with fixed buffer
        let read_e = opcode::ReadFixed::new(
            types::Fd(fd),
            std::ptr::null_mut(),
            size,
            buf_index,
        )
        .offset(offset)
        .build()
        .user_data(buf_index as u64);
        
        self.metrics.record_submission();
        
        let mut ring = self.ring.lock();
        unsafe {
            ring.submission()
                .push(&read_e)
                .map_err(|_| {
                    self.metrics.sq_full_events.fetch_add(1, Ordering::Relaxed);
                    CacheFsError::io_error("Submission queue full")
                })?;
        }
        
        ring.submit_and_wait(1)
            .map_err(|e| CacheFsError::io_error(format!("Submit failed: {}", e)))?;
            
        let cqe = ring.completion().next()
            .ok_or_else(|| CacheFsError::io_error("No completion event"))?;
            
        let result = cqe.result();
        if result < 0 {
            return Err(CacheFsError::io_error(format!(
                "Read failed: {}",
                io::Error::from_raw_os_error(-result)
            )));
        }
        
        let bytes_read = result as usize;
        let latency_us = start.elapsed().as_micros() as u64;
        self.metrics.record_completion(bytes_read, latency_us);
        
        Ok(bytes_read)
    }
    
    /// Perform zero-copy file transfer using splice
    pub async fn splice_file(
        &self,
        source: &Path,
        dest: &Path,
        size: u64,
    ) -> Result<()> {
        use nix::fcntl::{open, OFlag};
        use nix::sys::stat::Mode;
        use nix::unistd::pipe;
        
        let start = Instant::now();
        
        // Open source file
        let src_fd = open(source, OFlag::O_RDONLY, Mode::empty())
            .map_err(|e| CacheFsError::io_error(format!("Failed to open source: {}", e)))?;
            
        // Open/create destination file
        let dst_fd = open(
            dest,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
            Mode::from_bits_truncate(0o644),
        )
        .map_err(|e| CacheFsError::io_error(format!("Failed to open dest: {}", e)))?;
        
        // Create pipe for splice
        let (pipe_r, pipe_w) = pipe()
            .map_err(|e| CacheFsError::io_error(format!("Failed to create pipe: {}", e)))?;
            
        let mut offset = 0u64;
        let chunk_size = 16 * 1024 * 1024; // 16MB chunks
        
        info!(
            "Starting splice transfer: {} -> {} ({} bytes)",
            source.display(),
            dest.display(),
            size
        );
        
        while offset < size {
            let to_copy = (size - offset).min(chunk_size);
            
            // Splice from file to pipe
            let splice_in = opcode::Splice::new(
                types::Fd(src_fd),
                offset as i64,
                types::Fd(pipe_w),
                -1,
                to_copy as u32,
            )
            .build()
            .user_data(1);
            
            // Splice from pipe to file
            let splice_out = opcode::Splice::new(
                types::Fd(pipe_r),
                -1,
                types::Fd(dst_fd),
                offset as i64,
                to_copy as u32,
            )
            .build()
            .user_data(2);
            
            // Submit both operations
            let mut ring = self.ring.lock();
            unsafe {
                ring.submission()
                    .push(&splice_in)
                    .map_err(|_| CacheFsError::io_error("SQ full"))?;
                ring.submission()
                    .push(&splice_out)
                    .map_err(|_| CacheFsError::io_error("SQ full"))?;
            }
            
            ring.submit_and_wait(2)
                .map_err(|e| CacheFsError::io_error(format!("Submit failed: {}", e)))?;
                
            // Wait for both completions
            for _ in 0..2 {
                let cqe = ring.completion().next()
                    .ok_or_else(|| CacheFsError::io_error("No completion"))?;
                    
                if cqe.result() < 0 {
                    return Err(CacheFsError::io_error(format!(
                        "Splice failed: {}",
                        io::Error::from_raw_os_error(-cqe.result())
                    )));
                }
            }
            
            offset += to_copy;
            self.metrics.total_bytes_written.fetch_add(to_copy, Ordering::Relaxed);
        }
        
        let duration = start.elapsed();
        let throughput_mbps = (size as f64 / duration.as_secs_f64()) / (1024.0 * 1024.0);
        
        info!(
            "Splice transfer completed: {} bytes in {:?} ({:.1} MB/s)",
            size, duration, throughput_mbps
        );
        
        Ok(())
    }
    
    /// Get current metrics
    pub fn metrics(&self) -> &IoUringMetrics {
        &self.metrics
    }
    
    /// Check if io_uring is properly initialized
    pub fn is_ready(&self) -> bool {
        // Try to access the ring
        self.ring.try_lock().is_some()
    }
}