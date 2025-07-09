use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore, mpsc};
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

pub struct CacheManager {
    config: Arc<Config>,
    
    // 缓存状态管理
    cache_entries: Arc<DashMap<PathBuf, CacheEntry>>,
    
    // 驱逐策略
    eviction_policy: Arc<Mutex<Box<dyn EvictionPolicy>>>,
    
    // 任务管理
    task_queue: Arc<RwLock<std::collections::BinaryHeap<CacheTask>>>,
    active_tasks: Arc<DashMap<String, JoinHandle<Result<()>>>>,
    task_semaphore: Arc<Semaphore>,
    
    // 指标收集
    metrics: Arc<MetricsCollector>,
    
    // 任务通道
    task_sender: mpsc::UnboundedSender<CacheTask>,
    
    // 停止信号
    shutdown_sender: tokio::sync::watch::Sender<bool>,
    shutdown_receiver: tokio::sync::watch::Receiver<bool>,
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
        
        let manager = Self {
            config: Arc::clone(&config),
            cache_entries: Arc::new(DashMap::new()),
            eviction_policy: Arc::new(Mutex::new(eviction_policy)),
            task_queue: Arc::new(RwLock::new(std::collections::BinaryHeap::new())),
            active_tasks: Arc::new(DashMap::new()),
            task_semaphore: Arc::new(Semaphore::new(config.max_concurrent_caching as usize)),
            metrics: Arc::clone(&metrics),
            task_sender,
            shutdown_sender,
            shutdown_receiver,
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
        if let Some(mut entry) = self.cache_entries.get_mut(path) {
            entry.mark_accessed();
            self.eviction_policy.lock().on_access(path, &entry);
            self.metrics.record_cache_hit();
        } else {
            self.metrics.record_cache_miss();
        }
    }
    
    /// 提交缓存任务
    pub async fn submit_cache_task(&self, nfs_path: PathBuf, priority: CachePriority) -> Result<()> {
        let cache_path = self.get_cache_path(&nfs_path);
        
        // 检查是否已经在缓存或已缓存
        if self.is_cached(&cache_path) || self.is_caching(&cache_path) {
            return Ok(());
        }
        
        // 获取文件大小
        let file_size = match std::fs::metadata(&nfs_path) {
            Ok(metadata) => metadata.len(),
            Err(e) => {
                tracing::warn!("Failed to get file size for {}: {}", nfs_path.display(), e);
                return Err(CacheFsError::IoError(e));
            }
        };
        
        // 检查缓存空间
        if let Err(e) = self.ensure_cache_space(file_size).await {
            tracing::warn!("Failed to ensure cache space for {}: {}", nfs_path.display(), e);
            return Err(e);
        }
        
        // 创建缓存条目
        let mut entry = CacheEntry::new(file_size).with_priority(priority);
        let _progress = entry.start_caching(file_size);
        
        self.cache_entries.insert(cache_path.clone(), entry.clone());
        self.eviction_policy.lock().on_insert(cache_path.clone(), &entry);
        
        // 创建缓存任务
        let task = CacheTask::new(nfs_path, cache_path)
            .with_priority(priority)
            .with_file_size(file_size)
            .with_checksum(self.config.enable_checksums);
        
        // 提交任务
        if let Err(e) = self.task_sender.send(task) {
            tracing::error!("Failed to submit cache task: {}", e);
            return Err(CacheFsError::SendError(e.to_string()));
        }
        
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
    
    /// 驱逐文件以释放空间
    async fn evict_files(&self, space_needed: u64) -> Result<()> {
        let entries: HashMap<PathBuf, CacheEntry> = self.cache_entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        
        let victims = {
            let policy = self.eviction_policy.lock();
            policy.select_victims(&entries, space_needed)
        };
        
        let mut freed_space = 0u64;
        for victim_path in victims {
            if let Some((_, entry)) = self.cache_entries.remove(&victim_path) {
                // 删除缓存文件
                if let Err(e) = tokio::fs::remove_file(&victim_path).await {
                    tracing::warn!("Failed to remove cache file {}: {}", victim_path.display(), e);
                } else {
                    freed_space += entry.size;
                    self.eviction_policy.lock().on_remove(&victim_path);
                    self.metrics.record_eviction();
                    
                    tracing::debug!("Evicted cache file: {}", victim_path.display());
                }
            }
            
            if freed_space >= space_needed {
                break;
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
                                
                                let handle = tokio::spawn(async move {
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
    
    /// 执行缓存任务
    async fn execute_cache_task(
        mut task: CacheTask,
        cache_entries: Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: Arc<MetricsCollector>,
        config: Arc<Config>,
    ) -> Result<()> {
        let start_time = Instant::now();
        
        loop {
            let result = Self::copy_file_to_cache(&task, &cache_entries, &metrics, &config).await;
            
            match result {
                Ok(checksum) => {
                    // 缓存成功
                    if let Some(mut entry) = cache_entries.get_mut(&task.cache_path) {
                        entry.complete_caching(task.file_size.unwrap_or(0), checksum);
                        
                        // 原子性操作：重命名临时文件
                        let temp_path = task.get_temp_path();
                        if let Err(e) = tokio::fs::rename(&temp_path, &task.cache_path).await {
                            tracing::error!("Failed to rename cached file: {}", e);
                            entry.mark_failed(e.to_string(), task.retry_count);
                            metrics.record_cache_error();
                            return Err(CacheFsError::IoError(e));
                        }
                        
                        let latency = start_time.elapsed();
                        metrics.record_cache_operation(latency);
                        metrics.record_cache_task_complete();
                        
                        tracing::info!(
                            "Successfully cached file: {} (size: {}, time: {:?})",
                            task.source_path.display(),
                            task.file_size.unwrap_or(0),
                            latency
                        );
                        
                        return Ok(());
                    }
                }
                Err(e) => {
                    // 缓存失败
                    tracing::warn!("Cache task failed: {} (attempt {}/{}): {}", 
                        task.source_path.display(), task.retry_count + 1, task.max_retries, e);
                    
                    if task.can_retry() {
                        task.increment_retry();
                        
                        // 指数退避
                        let delay = Duration::from_millis(1000 * (1 << task.retry_count.min(5)));
                        tokio::time::sleep(delay).await;
                        
                        continue;
                    } else {
                        // 最终失败
                        if let Some(mut entry) = cache_entries.get_mut(&task.cache_path) {
                            entry.mark_failed(e.to_string(), task.retry_count);
                        }
                        
                        metrics.record_cache_error();
                        metrics.record_cache_task_complete();
                        
                        return Err(e);
                    }
                }
            }
        }
    }
    
    /// 复制文件到缓存
    async fn copy_file_to_cache(
        task: &CacheTask,
        cache_entries: &Arc<DashMap<PathBuf, CacheEntry>>,
        metrics: &Arc<MetricsCollector>,
        config: &Arc<Config>,
    ) -> Result<Option<String>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use sha2::{Sha256, Digest};
        
        let temp_path = task.get_temp_path();
        
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
        
        let mut buffer = vec![0u8; config.cache_block_size];
        let mut total_copied = 0u64;
        let mut hasher = if task.enable_checksum {
            Some(Sha256::new())
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
        
        // 复制文件
        loop {
            let bytes_read = source_file.read(&mut buffer).await
                .map_err(CacheFsError::IoError)?;
            
            if bytes_read == 0 {
                break;
            }
            
            let data = &buffer[..bytes_read];
            
            // 写入目标文件
            dest_file.write_all(data).await
                .map_err(CacheFsError::IoError)?;
            
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
        }
        
        // 确保数据写入磁盘
        dest_file.sync_all().await
            .map_err(CacheFsError::IoError)?;
        
        // 计算校验和
        let checksum = if let Some(hasher) = hasher {
            Some(format!("{:x}", hasher.finalize()))
        } else {
            None
        };
        
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