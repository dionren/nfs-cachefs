use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::cache::state::{CacheEntry, CacheStatus, CachePriority};
use crate::cache::task::CacheTask;
use crate::cache::eviction::{EvictionPolicy, LruEvictionPolicy};
use crate::cache::metrics::MetricsCollector;
use crate::core::config::Config;
use crate::core::error::CacheFsError;
use crate::Result;

#[cfg(feature = "io_uring")]
use crate::io::{IoUringExecutor, IoUringConfig};

/// 缓存管理器
pub struct CacheManager {
    config: Arc<Config>,
    
    // 缓存状态管理
    cache_entries: Arc<DashMap<PathBuf, CacheEntry>>,
    
    // 驱逐策略
    eviction_policy: Arc<Mutex<Box<dyn EvictionPolicy>>>,
    
    // 任务管理
    active_tasks: Arc<DashMap<String, JoinHandle<Result<()>>>>,
    task_semaphore: Arc<Semaphore>,
    
    // 指标收集
    metrics: Arc<MetricsCollector>,
    
    // 任务通道
    task_sender: mpsc::UnboundedSender<CacheTask>,
    
    // 停止信号
    shutdown_sender: tokio::sync::watch::Sender<bool>,
    shutdown_receiver: tokio::sync::watch::Receiver<bool>,
    
    // io_uring 执行器 (可选)
    #[cfg(feature = "io_uring")]
    io_uring_executor: Option<Arc<IoUringExecutor>>,
}

impl CacheManager {
    pub fn new(config: Arc<Config>, metrics: Arc<MetricsCollector>) -> Result<Self> {
        let eviction_policy: Box<dyn EvictionPolicy> = match config.eviction_policy {
            crate::core::config::EvictionPolicy::Lru => {
                Box::new(LruEvictionPolicy::new(10000))
            }
            crate::core::config::EvictionPolicy::Lfu => {
                Box::new(crate::cache::eviction::LfuEvictionPolicy::new())
            }
            crate::core::config::EvictionPolicy::Arc => {
                Box::new(crate::cache::eviction::ArcEvictionPolicy::new(10000))
            }
        };
        
        let (task_sender, task_receiver) = mpsc::unbounded_channel();
        let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
        
        // Initialize io_uring executor if enabled
        #[cfg(feature = "io_uring")]
        let io_uring_executor = if config.nvme.use_io_uring {
            tracing::info!("CacheManager: Initializing io_uring support...");
            
            if !crate::io::check_io_uring_support() {
                tracing::warn!("io_uring not supported, cache writes will use traditional I/O");
                None
            } else {
                let io_config = IoUringConfig {
                    queue_depth: config.nvme.queue_depth,
                    sq_poll: config.nvme.polling_mode,
                    io_poll: config.nvme.io_poll,
                    fixed_buffers: config.nvme.fixed_buffers,
                    huge_pages: config.nvme.use_hugepages,
                    sq_poll_idle: config.nvme.sq_poll_idle_ms,
                };
                
                match IoUringExecutor::new(io_config) {
                    Ok(executor) => {
                        tracing::info!("✅ CacheManager: io_uring initialized for cache writes");
                        Some(Arc::new(executor))
                    }
                    Err(e) => {
                        tracing::error!("CacheManager: Failed to initialize io_uring: {}", e);
                        None
                    }
                }
            }
        } else {
            None
        };
        
        let manager = Self {
            config: Arc::clone(&config),
            cache_entries: Arc::new(DashMap::new()),
            eviction_policy: Arc::new(Mutex::new(eviction_policy)),
            active_tasks: Arc::new(DashMap::new()),
            task_semaphore: Arc::new(Semaphore::new(config.max_concurrent_caching as usize)),
            metrics: Arc::clone(&metrics),
            task_sender,
            shutdown_sender,
            shutdown_receiver,
            #[cfg(feature = "io_uring")]
            io_uring_executor,
        };
        
        // 启动任务处理器
        manager.start_task_processor(task_receiver);
        
        Ok(manager)
    }
    
    /// 检查文件是否已缓存
    pub fn is_cached(&self, path: &PathBuf) -> bool {
        if let Some(entry) = self.cache_entries.get(path) {
            entry.status.is_cached()
        } else {
            false
        }
    }
    
    /// 检查文件是否正在缓存
    pub fn is_caching(&self, path: &PathBuf) -> bool {
        if let Some(entry) = self.cache_entries.get(path) {
            entry.status.is_caching()
        } else {
            false
        }
    }
    
    /// 获取缓存文件路径
    pub fn get_cache_path(&self, nfs_path: &PathBuf) -> PathBuf {
        let relative_path = nfs_path.strip_prefix(&self.config.nfs_backend_path)
            .unwrap_or(nfs_path);
        
        self.config.cache_dir.join(relative_path)
    }
    
    /// 记录文件访问
    pub fn record_access(&self, path: &PathBuf) {
        if self.is_cached(path) {
            self.metrics.record_cache_hit();
        } else {
            self.metrics.record_cache_miss();
        }
    }
    
    /// 提交缓存任务 - 修复竞态条件
    pub async fn submit_cache_task(&self, nfs_path: PathBuf, priority: CachePriority) -> Result<()> {
        let cache_path = self.get_cache_path(&nfs_path);
        
        // 获取文件大小
        let file_size = match tokio::fs::metadata(&nfs_path).await {
            Ok(metadata) => {
                let size = metadata.len();
                let size_mb = size as f64 / (1024.0 * 1024.0);
                tracing::info!("📊 CACHE TASK SUBMIT: {} ({:.1}MB) -> preparing to cache", 
                    nfs_path.display(), size_mb);
                size
            },
            Err(e) => {
                tracing::warn!("❌ CACHE TASK FAILED: Failed to get file size for {}: {}", nfs_path.display(), e);
                return Err(CacheFsError::IoError(e));
            }
        };
        
        // 使用原子操作避免竞态条件
        match self.cache_entries.entry(cache_path.clone()) {
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                // 文件已存在，检查状态
                match &entry.get().status {
                    CacheStatus::Cached { .. } => {
                        tracing::debug!("⏭️  CACHE SKIP: {} -> already cached", nfs_path.display());
                        return Ok(());
                    }
                    CacheStatus::CachingInProgress { .. } => {
                        tracing::debug!("⏭️  CACHE SKIP: {} -> already caching", nfs_path.display());
                        return Ok(());
                    }
                    CacheStatus::Failed { .. } | CacheStatus::NotCached => {
                        tracing::info!("🔄 CACHE RETRY: {} -> retrying failed cache", nfs_path.display());
                        // 失败状态或未缓存，可以重新缓存
                        // 继续执行后续逻辑
                    }
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                // 创建新的缓存条目
                let mut new_entry = CacheEntry::new(file_size).with_priority(priority);
                let _progress = new_entry.start_caching(file_size);
                entry.insert(new_entry);
                tracing::info!("📝 CACHE ENTRY CREATED: {} -> new cache entry", nfs_path.display());
            }
        }
        
        // 检查缓存空间
        if let Err(e) = self.ensure_cache_space(file_size).await {
            tracing::warn!("💾 CACHE SPACE INSUFFICIENT: {} -> {}", nfs_path.display(), e);
            // 如果空间不足，移除刚创建的条目
            self.cache_entries.remove(&cache_path);
            return Err(e);
        }
        
        // 确保条目处于缓存中状态，并通知驱逐策略
        if let Some(mut entry_ref) = self.cache_entries.get_mut(&cache_path) {
            if !entry_ref.status.is_caching() {
                let _progress = entry_ref.start_caching(file_size);
            }
            // 通知驱逐策略
            self.eviction_policy.lock().on_insert(cache_path.clone(), &*entry_ref);
        } else {
            // 条目不存在，这不应该发生
            return Err(CacheFsError::cache_error("Cache entry disappeared unexpectedly"));
        }
        
        // 创建缓存任务
        let task = CacheTask::new(nfs_path.clone(), cache_path.clone())
            .with_priority(priority)
            .with_checksum(self.config.enable_checksums)
            .with_file_size(file_size)
            .with_max_retries(3);
        
        tracing::info!("🚀 CACHE TASK STARTED: {} -> queued for background processing", nfs_path.display());
        
        // 提交任务到队列
        if let Err(e) = self.task_sender.send(task) {
            tracing::error!("❌ CACHE TASK QUEUE FAILED: {} -> {}", nfs_path.display(), e);
            // 清理缓存条目
            self.cache_entries.remove(&cache_path);
            return Err(CacheFsError::cache_error(format!("Failed to queue cache task: {}", e)));
        }
        
        // 记录缓存任务开始
        self.metrics.record_cache_task_start();
        
        Ok(())
    }
    
    /// 确保有足够的缓存空间
    async fn ensure_cache_space(&self, needed_size: u64) -> Result<()> {
        let current_size = self.get_current_cache_size();
        let available_space = self.config.max_cache_size_bytes.saturating_sub(current_size);
        
        if available_space >= needed_size {
            return Ok(());
        }
        
        // 需要驱逐一些文件
        let space_to_free = needed_size - available_space;
        self.evict_files(space_to_free).await?;
        
        Ok(())
    }
    
    /// 驱逐文件以释放空间 - 修复死锁风险
    async fn evict_files(&self, space_needed: u64) -> Result<()> {
        // 1. 首先收集所有缓存条目的快照
        let entries: HashMap<PathBuf, CacheEntry> = self.cache_entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        
        // 2. 计算驱逐候选者，避免长时间持有锁
        let victims = {
            let policy = self.eviction_policy.lock();
            policy.select_victims(&entries, space_needed)
        };
        
        // 3. 按顺序处理驱逐，避免死锁
        let mut freed_space = 0u64;
        let mut evicted_paths = Vec::new();
        
        for victim_path in victims {
            // 原子性检查和移除
            if let Some((_, entry)) = self.cache_entries.remove(&victim_path) {
                // 确保不驱逐正在缓存的文件
                if entry.status.is_caching() {
                    // 重新插入正在缓存的条目
                    self.cache_entries.insert(victim_path.clone(), entry);
                    continue;
                }
                
                // 删除缓存文件
                match tokio::fs::remove_file(&victim_path).await {
                    Ok(()) => {
                        freed_space += entry.size;
                        evicted_paths.push(victim_path.clone());
                        self.metrics.record_eviction();
                        
                        tracing::debug!("Evicted cache file: {}", victim_path.display());
                    }
                    Err(e) => {
                        tracing::warn!("Failed to remove cache file {}: {}", victim_path.display(), e);
                        // 如果文件删除失败，重新插入条目
                        self.cache_entries.insert(victim_path, entry);
                    }
                }
            }
            
            if freed_space >= space_needed {
                break;
            }
        }
        
        // 4. 批量通知驱逐策略，减少锁获取次数
        if !evicted_paths.is_empty() {
            let mut policy = self.eviction_policy.lock();
            for path in evicted_paths {
                policy.on_remove(&path);
            }
        }
        
        if freed_space < space_needed {
            return Err(CacheFsError::InsufficientSpace {
                needed: space_needed,
                available: freed_space,
            });
        }
        
        Ok(())
    }
    
    /// 获取当前缓存大小
    fn get_current_cache_size(&self) -> u64 {
        self.cache_entries
            .iter()
            .filter(|entry| entry.status.is_cached())
            .map(|entry| entry.size)
            .sum()
    }
    
    /// 启动任务处理器
    fn start_task_processor(&self, mut task_receiver: mpsc::UnboundedReceiver<CacheTask>) {
        let cache_entries = Arc::clone(&self.cache_entries);
        let active_tasks = Arc::clone(&self.active_tasks);
        let task_semaphore = Arc::clone(&self.task_semaphore);
        let metrics = Arc::clone(&self.metrics);
        let config = Arc::clone(&self.config);
        let mut shutdown_receiver = self.shutdown_receiver.clone();
        
        #[cfg(feature = "io_uring")]
        let io_uring_executor = self.io_uring_executor.clone();
        
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    task = task_receiver.recv() => {
                        match task {
                            Some(task) => {
                                let permit = match task_semaphore.clone().try_acquire_owned() {
                                    Ok(permit) => permit,
                                    Err(_) => {
                                        // 信号量已满，等待
                                        match task_semaphore.clone().acquire_owned().await {
                                            Ok(permit) => permit,
                                            Err(_) => continue,
                                        }
                                    }
                                };
                                
                                let task_id = task.id.clone();
                                let cache_entries = Arc::clone(&cache_entries);
                                let metrics = Arc::clone(&metrics);
                                let config = Arc::clone(&config);
                                
                                #[cfg(feature = "io_uring")]
                                let io_uring_exec = io_uring_executor.clone();
                                
                                let handle = tokio::spawn(async move {
                                    #[cfg(feature = "io_uring")]
                                    let result = Self::execute_cache_task(task, cache_entries, metrics, config, io_uring_exec).await;
                                    #[cfg(not(feature = "io_uring"))]
                                    let result = Self::execute_cache_task(task, cache_entries, metrics, config).await;
                                    drop(permit); // 释放信号量
                                    result
                                });
                                
                                active_tasks.insert(task_id, handle);
                            }
                            None => break,
                        }
                    }
                    _ = shutdown_receiver.changed() => {
                        if *shutdown_receiver.borrow() {
                            break;
                        }
                    }
                }
            }
        });
    }
    
    /// 执行缓存任务 - 修复原子性问题
    #[cfg(feature = "io_uring")]
    async fn execute_cache_task(
        mut task: CacheTask,
        cache_entries: Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: Arc<MetricsCollector>,
        config: Arc<Config>,
        io_uring_executor: Option<Arc<IoUringExecutor>>,
    ) -> Result<()> {
        Self::execute_cache_task_impl(task, cache_entries, metrics, config, io_uring_executor).await
    }
    
    #[cfg(not(feature = "io_uring"))]
    async fn execute_cache_task(
        mut task: CacheTask,
        cache_entries: Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: Arc<MetricsCollector>,
        config: Arc<Config>,
    ) -> Result<()> {
        Self::execute_cache_task_impl(task, cache_entries, metrics, config).await
    }
    
    /// 执行缓存任务实现
    async fn execute_cache_task_impl(
        mut task: CacheTask,
        cache_entries: Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: Arc<MetricsCollector>,
        config: Arc<Config>,
        #[cfg(feature = "io_uring")] io_uring_executor: Option<Arc<IoUringExecutor>>,
    ) -> Result<()> {
        let start_time = Instant::now();
        let file_size_mb = task.file_size.unwrap_or(0) as f64 / (1024.0 * 1024.0);
        
        tracing::info!("⚙️  CACHE TASK EXECUTE: {} ({:.1}MB) -> starting file copy", 
            task.source_path.display(), file_size_mb);
        
        // 确保在任何错误情况下都能清理临时文件
        struct TempFileGuard {
            temp_path: PathBuf,
        }
        
        impl Drop for TempFileGuard {
            fn drop(&mut self) {
                if self.temp_path.exists() {
                    if let Err(e) = std::fs::remove_file(&self.temp_path) {
                        tracing::warn!("⚠️  TEMP FILE CLEANUP FAILED: {}: {}", self.temp_path.display(), e);
                    } else {
                        tracing::debug!("🧹 TEMP FILE CLEANED: {}", self.temp_path.display());
                    }
                }
            }
        }
        
        loop {
            let temp_path = task.get_temp_path();
            let _temp_guard = TempFileGuard { temp_path: temp_path.clone() };
            
            tracing::debug!("📝 CACHE COPY START: {} -> {}", 
                task.source_path.display(), temp_path.display());
            
            let copy_start = Instant::now();
            #[cfg(feature = "io_uring")]
            let result = Self::copy_file_to_cache(&task, &cache_entries, &metrics, &config, &io_uring_executor).await;
            #[cfg(not(feature = "io_uring"))]
            let result = Self::copy_file_to_cache(&task, &cache_entries, &metrics, &config).await;
            let copy_duration = copy_start.elapsed();
            
            match result {
                Ok(checksum) => {
                    let copy_speed = if copy_duration.as_secs_f64() > 0.0 {
                        file_size_mb / copy_duration.as_secs_f64()
                    } else {
                        0.0
                    };
                    
                    tracing::info!("📋 CACHE COPY COMPLETE: {} -> copied in {:?} ({:.1} MB/s)", 
                        task.source_path.display(), copy_duration, copy_speed);
                    
                    // 原子性文件操作：先验证，再重命名
                    let file_size = task.file_size.unwrap_or(0);
                    
                    // 1. 验证临时文件完整性
                    match tokio::fs::metadata(&temp_path).await {
                        Ok(metadata) => {
                            if metadata.len() != file_size {
                                tracing::error!("❌ CACHE VERIFY FAILED: {} -> size mismatch (expected: {}, got: {})", 
                                    task.source_path.display(), file_size, metadata.len());
                                
                                if let Some(mut entry) = cache_entries.get_mut(&task.cache_path) {
                                    entry.mark_failed("File size mismatch".to_string(), task.retry_count);
                                }
                                
                                metrics.record_cache_error();
                                return Err(CacheFsError::cache_error("File size mismatch"));
                            } else {
                                tracing::debug!("✅ CACHE VERIFY OK: {} -> size check passed", task.source_path.display());
                            }
                        }
                        Err(e) => {
                            tracing::error!("❌ CACHE VERIFY FAILED: {} -> metadata error: {}", task.source_path.display(), e);
                            
                            if let Some(mut entry) = cache_entries.get_mut(&task.cache_path) {
                                entry.mark_failed(e.to_string(), task.retry_count);
                            }
                            
                            metrics.record_cache_error();
                            return Err(CacheFsError::IoError(e));
                        }
                    }
                    
                    // 2. 原子性重命名操作
                    tracing::debug!("🔄 CACHE RENAME: {} -> {}", temp_path.display(), task.cache_path.display());
                    let rename_start = Instant::now();
                    
                    match tokio::fs::rename(&temp_path, &task.cache_path).await {
                        Ok(()) => {
                            let rename_duration = rename_start.elapsed();
                            // 3. 更新缓存条目状态（必须在重命名成功后）
                            if let Some(mut entry) = cache_entries.get_mut(&task.cache_path) {
                                entry.complete_caching(file_size, checksum);
                                
                                let total_duration = start_time.elapsed();
                                let overall_speed = if total_duration.as_secs_f64() > 0.0 {
                                    file_size_mb / total_duration.as_secs_f64()
                                } else {
                                    0.0
                                };
                                
                                metrics.record_cache_operation(total_duration);
                                metrics.record_cache_task_complete();
                                
                                tracing::info!("🎉 CACHE TASK COMPLETE: {} ({:.1}MB) -> cached successfully! (copy: {:?}, rename: {:?}, total: {:?}, avg: {:.1} MB/s)",
                                    task.source_path.display(),
                                    file_size_mb,
                                    copy_duration,
                                    rename_duration,
                                    total_duration,
                                    overall_speed
                                );
                                
                                // 防止 Drop 清理已重命名的文件
                                std::mem::forget(_temp_guard);
                                return Ok(());
                            } else {
                                // 如果缓存条目丢失，这是一个严重错误
                                tracing::error!("❌ CACHE ENTRY LOST: {} -> cache entry disappeared during atomic operation", 
                                    task.cache_path.display());
                                return Err(CacheFsError::cache_error("Cache entry lost"));
                            }
                        }
                        Err(e) => {
                            let rename_duration = rename_start.elapsed();
                            tracing::error!("❌ CACHE RENAME FAILED: {} -> {} (after {:?}): {}", 
                                temp_path.display(), task.cache_path.display(), rename_duration, e);
                            
                            if let Some(mut entry) = cache_entries.get_mut(&task.cache_path) {
                                entry.mark_failed(e.to_string(), task.retry_count);
                            }
                            
                            metrics.record_cache_error();
                            return Err(CacheFsError::IoError(e));
                        }
                    }
                }
                Err(e) => {
                    // 缓存失败，临时文件会被自动清理
                    tracing::warn!("⚠️  CACHE COPY FAILED: {} (attempt {}/{}, after {:?}): {}", 
                        task.source_path.display(), task.retry_count + 1, task.max_retries, copy_duration, e);
                    
                    if task.can_retry() {
                        task.increment_retry();
                        
                        // 指数退避
                        let delay = Duration::from_millis(1000 * (1 << task.retry_count.min(5)));
                        tracing::info!("⏳ CACHE RETRY DELAY: {} -> retrying in {:?}", 
                            task.source_path.display(), delay);
                        tokio::time::sleep(delay).await;
                        
                        continue;
                    } else {
                        // 最终失败
                        if let Some(mut entry) = cache_entries.get_mut(&task.cache_path) {
                            entry.mark_failed(e.to_string(), task.retry_count);
                        }
                        
                        metrics.record_cache_error();
                        metrics.record_cache_task_complete();
                        
                        tracing::error!("❌ CACHE TASK FAILED: {} -> gave up after {} attempts", 
                            task.source_path.display(), task.max_retries + 1);
                        
                        return Err(e);
                    }
                }
            }
        }
    }
    
    /// 复制文件到缓存 - 优化版本
    #[cfg(feature = "io_uring")]
    async fn copy_file_to_cache(
        task: &CacheTask,
        cache_entries: &Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: &Arc<MetricsCollector>,
        config: &Arc<Config>,
        io_uring_executor: &Option<Arc<IoUringExecutor>>,
    ) -> Result<Option<String>> {
        // Check if io_uring is available and use it for large files
        if let Some(ref io_uring_exec) = io_uring_executor {
            // Use io_uring for files larger than 10MB for better performance
            let file_size = task.file_size.unwrap_or(0);
            if file_size > 10 * 1024 * 1024 && io_uring_exec.is_ready() {
                return Self::copy_file_with_io_uring(
                    task,
                    cache_entries,
                    metrics,
                    config,
                    io_uring_exec,
                ).await;
            }
        }
        
        // Fallback to regular async I/O
        Self::copy_file_with_async_io(task, cache_entries, metrics, config, io_uring_executor).await
    }
    
    #[cfg(not(feature = "io_uring"))]
    async fn copy_file_to_cache(
        task: &CacheTask,
        cache_entries: &Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: &Arc<MetricsCollector>,
        config: &Arc<Config>,
    ) -> Result<Option<String>> {
        Self::copy_file_with_async_io(task, cache_entries, metrics, config).await
    }
    
    /// 使用 io_uring 复制文件 - 零拷贝实现
    #[cfg(feature = "io_uring")]
    async fn copy_file_with_io_uring(
        task: &CacheTask,
        cache_entries: &Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: &Arc<MetricsCollector>,
        config: &Arc<Config>,
        io_uring_executor: &Arc<IoUringExecutor>,
    ) -> Result<Option<String>> {
        let temp_path = task.get_temp_path();
        let file_size = task.file_size.unwrap_or(0);
        let file_size_mb = file_size as f64 / (1024.0 * 1024.0);
        
        tracing::info!("🚀 CACHE IO_URING: {} ({:.1}MB) -> using zero-copy splice", 
            task.source_path.display(), file_size_mb);
        
        // 确保父目录存在
        if let Some(parent) = temp_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Err(CacheFsError::IoError(e));
            }
        }
        
        let copy_start = std::time::Instant::now();
        
        // 获取进度跟踪器
        let progress = if let Some(entry) = cache_entries.get(&task.cache_path) {
            if let CacheStatus::CachingInProgress { progress, .. } = &entry.status {
                Some(Arc::clone(progress))
            } else {
                None
            }
        } else {
            None
        };
        
        // Use io_uring splice for zero-copy transfer
        match io_uring_executor.splice_file(&task.source_path, &temp_path, file_size).await {
            Ok(()) => {
                let copy_duration = copy_start.elapsed();
                let copy_speed = if copy_duration.as_secs_f64() > 0.0 {
                    file_size_mb / copy_duration.as_secs_f64()
                } else {
                    0.0
                };
                
                tracing::info!("✨ CACHE IO_URING COMPLETE: {} -> spliced in {:?} ({:.1} MB/s)", 
                    task.source_path.display(), copy_duration, copy_speed);
                
                // Update progress to 100%
                if let Some(ref progress) = progress {
                    progress.store(file_size, std::sync::atomic::Ordering::Relaxed);
                }
                
                // Record metrics
                metrics.record_nfs_read(file_size);
                
                // Calculate checksum if needed (requires reading the file)
                let checksum = if task.enable_checksum {
                    // For checksums, we need to read the file
                    // This is a tradeoff - we lose zero-copy benefit but gain integrity checking
                    tracing::debug!("🔐 CACHE CHECKSUM: calculating for {}", task.source_path.display());
                    Self::calculate_file_checksum(&temp_path).await?
                } else {
                    None
                };
                
                Ok(checksum)
            }
            Err(e) => {
                tracing::error!("❌ CACHE IO_URING FAILED: {} -> {}", task.source_path.display(), e);
                // Fall back to regular async I/O
                tracing::info!("🔄 CACHE FALLBACK: {} -> using regular async I/O", task.source_path.display());
                Self::copy_file_with_async_io(task, cache_entries, metrics, config, &Some(Arc::clone(io_uring_exec))).await
            }
        }
    }
    
    /// 计算文件校验和
    async fn calculate_file_checksum(path: &std::path::Path) -> Result<Option<String>> {
        use tokio::io::AsyncReadExt;
        use sha2::{Sha256, Digest};
        
        let mut file = tokio::fs::File::open(path).await.map_err(CacheFsError::IoError)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024]; // 1MB buffer
        
        loop {
            let bytes_read = file.read(&mut buffer).await.map_err(CacheFsError::IoError)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }
        
        let checksum = format!("{:x}", hasher.finalize());
        Ok(Some(checksum))
    }
    
    /// 使用常规异步 I/O 复制文件 - 原始实现
    async fn copy_file_with_async_io(
        task: &CacheTask,
        cache_entries: &Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: &Arc<MetricsCollector>,
        config: &Arc<Config>,
        #[cfg(feature = "io_uring")]
        _io_uring_executor: &Option<Arc<IoUringExecutor>>,
    ) -> Result<Option<String>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncSeekExt};
        use sha2::{Sha256, Digest};
        
        let temp_path = task.get_temp_path();
        let file_size = task.file_size.unwrap_or(0);
        let file_size_mb = file_size as f64 / (1024.0 * 1024.0);
        
        // 确保父目录存在
        if let Some(parent) = temp_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Err(CacheFsError::IoError(e));
            }
        }
        
        // 打开源文件和目标文件
        let mut source_file = tokio::fs::File::open(&task.source_path).await
            .map_err(CacheFsError::IoError)?;
        let mut dest_file = tokio::fs::File::create(&temp_path).await
            .map_err(CacheFsError::IoError)?;
        
        // 智能选择缓冲区大小 - 优化版本
        let buffer_size = if file_size < 1024 * 1024 { // 1MB以下
            // 小文件直接一次性读取，避免分块
            std::cmp::min(file_size as usize, 1024 * 1024)
        } else if file_size < 64 * 1024 * 1024 { // 64MB以下
            // 中等文件使用2MB块
            2 * 1024 * 1024
        } else {
            // 大文件使用配置的块大小
            config.cache_block_size
        };
        
        let mut buffer = vec![0u8; buffer_size];
        let mut total_copied = 0u64;
        let mut hasher = if task.enable_checksum {
            Some(sha2::Sha256::new())
        } else {
            None
        };
        
        // 获取进度跟踪器
        let progress = if let Some(entry) = cache_entries.get(&task.cache_path) {
            if let CacheStatus::CachingInProgress { progress, .. } = &entry.status {
                Some(Arc::clone(progress))
            } else {
                None
            }
        } else {
            None
        };
        
        let copy_start = std::time::Instant::now();
        let mut last_progress_log = copy_start;
        let progress_log_interval = std::time::Duration::from_secs(2);
        
        // 针对小文件的优化：批量复制
        if file_size < 1024 * 1024 { // 1MB以下的小文件
            tracing::info!("🚀 CACHE SMALL FILE: {} ({:.1}KB) -> single-pass copy", 
                task.source_path.display(), file_size as f64 / 1024.0);
            
            match source_file.read_exact(&mut buffer[..file_size as usize]).await {
                Ok(_) => {
                    let data = &buffer[..file_size as usize];
                    
                    // 计算校验和
                    if let Some(ref mut hasher) = hasher {
                        hasher.update(data);
                    }
                    
                    // 一次性写入
                    dest_file.write_all(data).await.map_err(CacheFsError::IoError)?;
                    total_copied = file_size;
                    
                    // 更新进度
                    if let Some(ref progress) = progress {
                        progress.store(file_size, std::sync::atomic::Ordering::Relaxed);
                    }
                    
                    metrics.record_nfs_read(file_size);
                }
                Err(e) => {
                    // 如果精确读取失败，回退到常规方法
                    tracing::debug!("🔄 CACHE FALLBACK: {} -> using chunked copy due to: {}", 
                        task.source_path.display(), e);
                    
                                                             // 重新定位到文件开头
                    use tokio::io::AsyncSeekExt;
                    source_file.seek(tokio::io::SeekFrom::Start(0)).await.map_err(CacheFsError::IoError)?;
                    
                    // 使用常规分块复制
                    #[cfg(feature = "io_uring")]
                    return Self::copy_file_chunked(task, cache_entries, metrics, config, 
                        source_file, dest_file, buffer, hasher, progress, io_uring_executor).await;
                    #[cfg(not(feature = "io_uring"))]
                    return Self::copy_file_chunked(task, cache_entries, metrics, config, 
                        source_file, dest_file, buffer, hasher, progress).await;
                }
            }
        } else {
            // 大文件使用分块复制
            #[cfg(feature = "io_uring")]
            return Self::copy_file_chunked(task, cache_entries, metrics, config, 
                source_file, dest_file, buffer, hasher, progress, io_uring_executor).await;
            #[cfg(not(feature = "io_uring"))]
            return Self::copy_file_chunked(task, cache_entries, metrics, config, 
                source_file, dest_file, buffer, hasher, progress).await;
        }
        
        // 确保数据写入磁盘
        tracing::debug!("💾 CACHE SYNC: {} -> flushing to disk", task.source_path.display());
        dest_file.sync_all().await.map_err(CacheFsError::IoError)?;
        
        // 计算校验和
        let checksum = if let Some(hasher) = hasher {
            let checksum_str = format!("{:x}", hasher.finalize());
            tracing::debug!("🔐 CACHE CHECKSUM: {} -> {}", task.source_path.display(), &checksum_str[..16]);
            Some(checksum_str)
        } else {
            None
        };
        
        let total_time = copy_start.elapsed();
        let final_speed = if total_time.as_secs_f64() > 0.0 {
            file_size_mb / total_time.as_secs_f64()
        } else {
            0.0
        };
        
        tracing::info!("📊 CACHE COPY COMPLETE: {} -> {:.1}MB in {:?} ({:.1} MB/s)", 
            task.source_path.display(), file_size_mb, total_time, final_speed);
        
        Ok(checksum)
    }
    
    /// 分块复制文件 - 独立函数
    async fn copy_file_chunked(
        task: &CacheTask,
        cache_entries: &Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: &Arc<MetricsCollector>,
        config: &Arc<Config>,
        mut source_file: tokio::fs::File,
        mut dest_file: tokio::fs::File,
        mut buffer: Vec<u8>,
        mut hasher: Option<sha2::Sha256>,
        progress: Option<Arc<std::sync::atomic::AtomicU64>>,
        #[cfg(feature = "io_uring")] _io_uring_executor: &Option<Arc<IoUringExecutor>>,
    ) -> Result<Option<String>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use sha2::Digest;
        
        let file_size = task.file_size.unwrap_or(0);
        let file_size_mb = file_size as f64 / (1024.0 * 1024.0);
        let mut total_copied = 0u64;
        
        let copy_start = std::time::Instant::now();
        let mut last_progress_log = copy_start;
        let progress_log_interval = std::time::Duration::from_secs(2);
        
        tracing::info!("🔄 CACHE CHUNKED COPY: {} ({:.1}MB) -> chunked copy with {:.1}MB blocks", 
            task.source_path.display(), file_size_mb, buffer.len() as f64 / (1024.0 * 1024.0));
        
        // 分块复制
        loop {
            let bytes_read = source_file.read(&mut buffer).await
                .map_err(CacheFsError::IoError)?;
            
            if bytes_read == 0 {
                break;
            }
            
            let data = &buffer[..bytes_read];
            
            // 写入目标文件
            dest_file.write_all(data).await.map_err(CacheFsError::IoError)?;
            
            // 更新校验和
            if let Some(ref mut hasher) = hasher {
                hasher.update(data);
            }
            
            total_copied += bytes_read as u64;
            
            // 更新进度
            if let Some(ref progress) = progress {
                progress.store(total_copied, std::sync::atomic::Ordering::Relaxed);
            }
            
            // 记录NFS读取
            metrics.record_nfs_read(bytes_read as u64);
            
            // 定期打印进度日志（仅对大文件）
            let now = std::time::Instant::now();
            if file_size_mb > 10.0 && now.duration_since(last_progress_log) >= progress_log_interval {
                let progress_percent = if file_size > 0 {
                    (total_copied as f64 / file_size as f64) * 100.0
                } else {
                    0.0
                };
                let elapsed = now.duration_since(copy_start);
                let speed_mbps = if elapsed.as_secs_f64() > 0.0 {
                    (total_copied as f64) / (1024.0 * 1024.0) / elapsed.as_secs_f64()
                } else {
                    0.0
                };
                let copied_mb = total_copied as f64 / (1024.0 * 1024.0);
                
                tracing::info!("📈 CACHE PROGRESS: {} -> {:.1}% ({:.1}/{:.1}MB, {:.1} MB/s)", 
                    task.source_path.display(), progress_percent, copied_mb, file_size_mb, speed_mbps);
                
                last_progress_log = now;
            }
        }
        
        // 确保数据写入磁盘
        tracing::debug!("💾 CACHE SYNC: {} -> flushing to disk", task.source_path.display());
        dest_file.sync_all().await.map_err(CacheFsError::IoError)?;
        
        // 计算校验和
        let checksum = if let Some(hasher) = hasher {
            let checksum_str = format!("{:x}", hasher.finalize());
            tracing::debug!("🔐 CACHE CHECKSUM: {} -> {}", task.source_path.display(), &checksum_str[..16]);
            Some(checksum_str)
        } else {
            None
        };
        
        let total_time = copy_start.elapsed();
        let final_speed = if total_time.as_secs_f64() > 0.0 {
            file_size_mb / total_time.as_secs_f64()
        } else {
            0.0
        };
        
        tracing::debug!("📊 CACHE COPY STATS: {} -> {:.1}MB in {:?} ({:.1} MB/s)", 
            task.source_path.display(), file_size_mb, total_time, final_speed);
        
        Ok(checksum)
    }
    
    /// 更新缓存统计信息
    pub fn update_metrics(&self) {
        let mut cached_count = 0u64;
        let mut caching_count = 0u64;
        let mut failed_count = 0u64;
        let mut total_size = 0u64;
        
        for entry in self.cache_entries.iter() {
            match &entry.status {
                CacheStatus::Cached { .. } => {
                    cached_count += 1;
                    total_size += entry.size;
                }
                CacheStatus::CachingInProgress { .. } => {
                    caching_count += 1;
                }
                CacheStatus::Failed { .. } => {
                    failed_count += 1;
                }
                _ => {}
            }
        }
        
        self.metrics.update_file_counts(cached_count, caching_count, failed_count);
        self.metrics.update_cache_size(self.config.max_cache_size_bytes, total_size);
    }
    
    /// 获取缓存统计信息
    pub fn get_cache_stats(&self) -> HashMap<String, u64> {
        let mut stats = HashMap::new();
        
        for entry in self.cache_entries.iter() {
            let status_key = match &entry.status {
                CacheStatus::NotCached => "not_cached",
                CacheStatus::CachingInProgress { .. } => "caching",
                CacheStatus::Cached { .. } => "cached",
                CacheStatus::Failed { .. } => "failed",
            };
            
            *stats.entry(status_key.to_string()).or_insert(0) += 1;
        }
        
        stats
    }
    
    /// 清理过期缓存
    pub async fn cleanup_expired_cache(&self) -> Result<()> {
        if let Some(ttl_seconds) = self.config.cache_ttl_seconds {
            let now = std::time::SystemTime::now();
            let mut expired_paths = Vec::new();
            
            for entry in self.cache_entries.iter() {
                if let CacheStatus::Cached { cached_at, .. } = &entry.status {
                    if let Ok(age) = now.duration_since(*cached_at) {
                        if age.as_secs() > ttl_seconds {
                            expired_paths.push(entry.key().clone());
                        }
                    }
                }
            }
            
            for path in expired_paths {
                if let Some((_, _entry)) = self.cache_entries.remove(&path) {
                    if let Err(e) = tokio::fs::remove_file(&path).await {
                        tracing::warn!("Failed to remove expired cache file {}: {}", path.display(), e);
                    } else {
                        self.eviction_policy.lock().on_remove(&path);
                        tracing::debug!("Removed expired cache file: {}", path.display());
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// 停止缓存管理器
    pub async fn shutdown(&self) -> Result<()> {
        // 发送停止信号
        let _ = self.shutdown_sender.send(true);
        
        // 等待所有活动任务完成
        let active_tasks: Vec<_> = self.active_tasks.iter().map(|entry| entry.key().clone()).collect();
        
        for task_id in active_tasks {
            if let Some((_, handle)) = self.active_tasks.remove(&task_id) {
                let _ = handle.await;
            }
        }
        
        tracing::info!("Cache manager shutdown completed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::time::Duration;
    
    fn create_test_config() -> Config {
        let temp_dir = TempDir::new().unwrap();
        let mut config = Config::default();
        config.cache_dir = temp_dir.path().to_path_buf();
        config.nfs_backend_path = temp_dir.path().join("nfs");
        config.max_cache_size_bytes = 1024 * 1024; // 1MB
        config.cache_block_size = 1024;
        config.max_concurrent_caching = 2;
        config.allow_async_read = false;
        config
    }
    
    #[tokio::test]
    async fn test_cache_manager_creation() {
        let config = Arc::new(create_test_config());
        let metrics = Arc::new(MetricsCollector::new());
        
        let manager = CacheManager::new(config, metrics).unwrap();
        assert_eq!(manager.cache_entries.len(), 0);
    }
    
    #[tokio::test]
    async fn test_cache_path_generation() {
        let config = Arc::new(create_test_config());
        let metrics = Arc::new(MetricsCollector::new());
        let manager = CacheManager::new(config.clone(), metrics).unwrap();
        
        let nfs_path = config.nfs_backend_path.join("test.txt");
        let cache_path = manager.get_cache_path(&nfs_path);
        
        assert_eq!(cache_path, config.cache_dir.join("test.txt"));
    }
    
    #[tokio::test]
    async fn test_cache_space_management() {
        let config = Arc::new(create_test_config());
        let metrics = Arc::new(MetricsCollector::new());
        let manager = CacheManager::new(config, metrics).unwrap();
        
        // 测试空间检查
        let result = manager.ensure_cache_space(500).await;
        assert!(result.is_ok());
        
        // 测试空间不足
        let result = manager.ensure_cache_space(2 * 1024 * 1024).await; // 2MB
        assert!(result.is_err());
    }
} 