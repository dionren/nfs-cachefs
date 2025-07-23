use std::alloc::{alloc, dealloc, Layout};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::VecDeque;

use parking_lot::Mutex;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::Result;
use crate::core::error::CacheFsError;

/// Aligned buffer for Direct I/O
pub struct AlignedBuffer {
    ptr: NonNull<u8>,
    size: usize,
    alignment: usize,
    layout: Layout,
}

impl AlignedBuffer {
    /// Create a new aligned buffer
    pub fn new(size: usize, alignment: usize) -> Result<Self> {
        // Ensure alignment is power of 2
        if !alignment.is_power_of_two() {
            return Err(CacheFsError::config_error("Alignment must be power of 2"));
        }
        
        // Create layout
        let layout = Layout::from_size_align(size, alignment)
            .map_err(|e| CacheFsError::config_error(format!("Invalid layout: {}", e)))?;
            
        // Allocate memory
        let ptr = unsafe { alloc(layout) };
        let ptr = NonNull::new(ptr)
            .ok_or_else(|| CacheFsError::memory_error("Failed to allocate aligned buffer"))?;
            
        debug!("Allocated aligned buffer: size={}, alignment={}", size, alignment);
        
        Ok(Self {
            ptr,
            size,
            alignment,
            layout,
        })
    }
    
    /// Get buffer as mutable pointer
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
    
    /// Get buffer as slice
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.size) }
    }
    
    /// Get buffer as mutable slice
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.size) }
    }
    
    /// Copy data to vector
    pub fn to_vec(&self, len: usize) -> Vec<u8> {
        let len = len.min(self.size);
        self.as_slice()[..len].to_vec()
    }
    
    /// Get buffer size
    pub fn size(&self) -> usize {
        self.size
    }
    
    /// Clear buffer (zero out)
    pub fn clear(&mut self) {
        unsafe {
            std::ptr::write_bytes(self.ptr.as_ptr(), 0, self.size);
        }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.ptr.as_ptr(), self.layout);
        }
        debug!("Deallocated aligned buffer: size={}", self.size);
    }
}

// Safety: AlignedBuffer can be sent between threads
unsafe impl Send for AlignedBuffer {}
unsafe impl Sync for AlignedBuffer {}

/// Buffer pool for io_uring operations
pub struct BufferPool {
    buffers: Vec<Arc<Mutex<AlignedBuffer>>>,
    free_list: Arc<Mutex<VecDeque<usize>>>,
    semaphore: Arc<Semaphore>,
    buffer_size: usize,
    total_buffers: usize,
    allocated: AtomicUsize,
}

impl BufferPool {
    /// Create a new buffer pool
    pub fn new(count: usize, buffer_size: usize, use_huge_pages: bool) -> Result<Self> {
        if count == 0 {
            return Err(CacheFsError::config_error("Buffer pool count must be > 0"));
        }
        
        let alignment = if use_huge_pages {
            2 * 1024 * 1024 // 2MB huge page alignment
        } else {
            4096 // Standard page alignment
        };
        
        let mut buffers = Vec::with_capacity(count);
        let mut free_list = VecDeque::with_capacity(count);
        
        // Pre-allocate all buffers
        for i in 0..count {
            let buffer = AlignedBuffer::new(buffer_size, alignment)?;
            buffers.push(Arc::new(Mutex::new(buffer)));
            free_list.push_back(i);
        }
        
        debug!(
            "Created buffer pool: {} buffers of {} bytes (alignment: {})",
            count, buffer_size, alignment
        );
        
        Ok(Self {
            buffers,
            free_list: Arc::new(Mutex::new(free_list)),
            semaphore: Arc::new(Semaphore::new(count)),
            buffer_size,
            total_buffers: count,
            allocated: AtomicUsize::new(0),
        })
    }
    
    /// Acquire a buffer from the pool
    pub async fn acquire(&self) -> Result<BufferGuard> {
        // Wait for available buffer
        let permit = self.semaphore.acquire().await
            .map_err(|_| CacheFsError::resource_error("Failed to acquire buffer permit"))?;
            
        // Get buffer index from free list
        let index = {
            let mut free_list = self.free_list.lock();
            free_list.pop_front()
                .ok_or_else(|| CacheFsError::resource_error("No free buffers available"))?
        };
        
        self.allocated.fetch_add(1, Ordering::Relaxed);
        
        let buffer = self.buffers[index].clone();
        
        Ok(BufferGuard {
            buffer,
            index,
            pool: self,
            _permit: permit,
        })
    }
    
    /// Release a buffer back to the pool
    fn release(&self, index: usize) {
        let mut free_list = self.free_list.lock();
        free_list.push_back(index);
        self.allocated.fetch_sub(1, Ordering::Relaxed);
    }
    
    /// Get number of allocated buffers
    pub fn allocated_count(&self) -> usize {
        self.allocated.load(Ordering::Relaxed)
    }
    
    /// Get total number of buffers
    pub fn total_count(&self) -> usize {
        self.total_buffers
    }
    
    /// Get buffer size
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }
    
    /// Register all buffers with io_uring (for fixed buffers)
    pub fn get_iovecs(&self) -> Vec<libc::iovec> {
        self.buffers.iter().map(|buffer| {
            let buf = buffer.lock();
            libc::iovec {
                iov_base: buf.as_mut_ptr() as *mut libc::c_void,
                iov_len: buf.size(),
            }
        }).collect()
    }
}

/// RAII guard for buffer pool
pub struct BufferGuard<'a> {
    buffer: Arc<Mutex<AlignedBuffer>>,
    index: usize,
    pool: &'a BufferPool,
    _permit: tokio::sync::SemaphorePermit<'a>,
}

impl<'a> BufferGuard<'a> {
    /// Get buffer index (for fixed buffer operations)
    pub fn index(&self) -> usize {
        self.index
    }
    
    /// Access the buffer
    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&AlignedBuffer) -> R,
    {
        let buffer = self.buffer.lock();
        f(&*buffer)
    }
    
    /// Access the buffer mutably
    pub fn with_mut<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut AlignedBuffer) -> R,
    {
        let mut buffer = self.buffer.lock();
        f(&mut *buffer)
    }
    
    /// Get buffer as mutable pointer
    pub fn as_mut_ptr(&self) -> *mut u8 {
        let buffer = self.buffer.lock();
        buffer.as_mut_ptr()
    }
    
    /// Get buffer size
    pub fn size(&self) -> usize {
        self.pool.buffer_size
    }
    
    /// Copy data to vector
    pub fn to_vec(&self, len: usize) -> Vec<u8> {
        let buffer = self.buffer.lock();
        buffer.to_vec(len)
    }
}

impl<'a> Drop for BufferGuard<'a> {
    fn drop(&mut self) {
        // Clear buffer before returning to pool
        {
            let mut buffer = self.buffer.lock();
            buffer.clear();
        }
        
        // Return to free list
        self.pool.release(self.index);
        
        debug!("Released buffer {} back to pool", self.index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_aligned_buffer() {
        let buffer = AlignedBuffer::new(4096, 512).unwrap();
        assert_eq!(buffer.size(), 4096);
        
        // Check alignment
        let ptr = buffer.as_mut_ptr() as usize;
        assert_eq!(ptr % 512, 0);
    }
    
    #[tokio::test]
    async fn test_buffer_pool() {
        let pool = BufferPool::new(4, 1024, false).unwrap();
        
        // Acquire all buffers
        let mut guards = Vec::new();
        for i in 0..4 {
            let guard = pool.acquire().await.unwrap();
            assert_eq!(guard.index(), i);
            guards.push(guard);
        }
        
        assert_eq!(pool.allocated_count(), 4);
        
        // Drop one guard
        guards.pop();
        assert_eq!(pool.allocated_count(), 3);
        
        // Can acquire again
        let _guard = pool.acquire().await.unwrap();
        assert_eq!(pool.allocated_count(), 4);
    }
}