use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetrics {
    // 缓存命中统计
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_hit_rate: f64,
    
    // 缓存操作统计
    pub total_reads: u64,
    pub total_writes: u64,
    pub total_evictions: u64,
    pub total_cache_tasks: u64,
    pub active_cache_tasks: u64,
    
    // 性能统计
    pub avg_read_latency_ms: f64,
    pub avg_write_latency_ms: f64,
    pub avg_cache_latency_ms: f64,
    
    // 容量统计
    pub total_cache_size: u64,
    pub used_cache_size: u64,
    pub cache_utilization: f64,
    pub available_cache_size: u64,
    
    // 文件统计
    pub cached_files_count: u64,
    pub caching_files_count: u64,
    pub failed_files_count: u64,
    
    // 网络统计
    pub nfs_bytes_read: u64,
    pub nfs_bytes_written: u64,
    pub nfs_operations: u64,
    
    // 错误统计
    pub cache_errors: u64,
    pub nfs_errors: u64,
    pub checksum_errors: u64,
    
    // 时间戳
    pub last_updated: SystemTime,
    pub uptime_seconds: u64,
}

impl Default for CacheMetrics {
    fn default() -> Self {
        Self {
            cache_hits: 0,
            cache_misses: 0,
            cache_hit_rate: 0.0,
            total_reads: 0,
            total_writes: 0,
            total_evictions: 0,
            total_cache_tasks: 0,
            active_cache_tasks: 0,
            avg_read_latency_ms: 0.0,
            avg_write_latency_ms: 0.0,
            avg_cache_latency_ms: 0.0,
            total_cache_size: 0,
            used_cache_size: 0,
            cache_utilization: 0.0,
            available_cache_size: 0,
            cached_files_count: 0,
            caching_files_count: 0,
            failed_files_count: 0,
            nfs_bytes_read: 0,
            nfs_bytes_written: 0,
            nfs_operations: 0,
            cache_errors: 0,
            nfs_errors: 0,
            checksum_errors: 0,
            last_updated: SystemTime::now(),
            uptime_seconds: 0,
        }
    }
}

pub struct MetricsCollector {
    start_time: Instant,
    
    // 原子计数器
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    total_reads: AtomicU64,
    total_writes: AtomicU64,
    total_evictions: AtomicU64,
    total_cache_tasks: AtomicU64,
    active_cache_tasks: AtomicU64,
    
    nfs_bytes_read: AtomicU64,
    nfs_bytes_written: AtomicU64,
    nfs_operations: AtomicU64,
    
    cache_errors: AtomicU64,
    nfs_errors: AtomicU64,
    checksum_errors: AtomicU64,
    
    cached_files_count: AtomicU64,
    caching_files_count: AtomicU64,
    failed_files_count: AtomicU64,
    
    // 延迟统计
    read_latencies: Arc<RwLock<Vec<Duration>>>,
    write_latencies: Arc<RwLock<Vec<Duration>>>,
    cache_latencies: Arc<RwLock<Vec<Duration>>>,
    
    // 容量信息
    total_cache_size: AtomicU64,
    used_cache_size: AtomicU64,
    
    // 历史数据
    historical_metrics: Arc<RwLock<Vec<CacheMetrics>>>,
    max_history_size: usize,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            total_reads: AtomicU64::new(0),
            total_writes: AtomicU64::new(0),
            total_evictions: AtomicU64::new(0),
            total_cache_tasks: AtomicU64::new(0),
            active_cache_tasks: AtomicU64::new(0),
            nfs_bytes_read: AtomicU64::new(0),
            nfs_bytes_written: AtomicU64::new(0),
            nfs_operations: AtomicU64::new(0),
            cache_errors: AtomicU64::new(0),
            nfs_errors: AtomicU64::new(0),
            checksum_errors: AtomicU64::new(0),
            cached_files_count: AtomicU64::new(0),
            caching_files_count: AtomicU64::new(0),
            failed_files_count: AtomicU64::new(0),
            read_latencies: Arc::new(RwLock::new(Vec::new())),
            write_latencies: Arc::new(RwLock::new(Vec::new())),
            cache_latencies: Arc::new(RwLock::new(Vec::new())),
            total_cache_size: AtomicU64::new(0),
            used_cache_size: AtomicU64::new(0),
            historical_metrics: Arc::new(RwLock::new(Vec::new())),
            max_history_size: 1000,
        }
    }
    
    // 缓存命中统计
    pub fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }
    
    // 操作统计
    pub fn record_read(&self, latency: Duration) {
        self.total_reads.fetch_add(1, Ordering::Relaxed);
        self.read_latencies.write().push(latency);
    }
    
    pub fn record_write(&self, latency: Duration) {
        self.total_writes.fetch_add(1, Ordering::Relaxed);
        self.write_latencies.write().push(latency);
    }
    
    pub fn record_cache_operation(&self, latency: Duration) {
        self.cache_latencies.write().push(latency);
    }
    
    pub fn record_eviction(&self) {
        self.total_evictions.fetch_add(1, Ordering::Relaxed);
    }
    
    // 缓存任务统计
    pub fn record_cache_task_start(&self) {
        self.total_cache_tasks.fetch_add(1, Ordering::Relaxed);
        self.active_cache_tasks.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_cache_task_complete(&self) {
        self.active_cache_tasks.fetch_sub(1, Ordering::Relaxed);
    }
    
    // NFS 统计
    pub fn record_nfs_read(&self, bytes: u64) {
        self.nfs_bytes_read.fetch_add(bytes, Ordering::Relaxed);
        self.nfs_operations.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_nfs_write(&self, bytes: u64) {
        self.nfs_bytes_written.fetch_add(bytes, Ordering::Relaxed);
        self.nfs_operations.fetch_add(1, Ordering::Relaxed);
    }
    
    // 错误统计
    pub fn record_cache_error(&self) {
        self.cache_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_nfs_error(&self) {
        self.nfs_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_checksum_error(&self) {
        self.checksum_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    // 文件状态统计
    pub fn update_file_counts(&self, cached: u64, caching: u64, failed: u64) {
        self.cached_files_count.store(cached, Ordering::Relaxed);
        self.caching_files_count.store(caching, Ordering::Relaxed);
        self.failed_files_count.store(failed, Ordering::Relaxed);
    }
    
    // 容量统计
    pub fn update_cache_size(&self, total: u64, used: u64) {
        self.total_cache_size.store(total, Ordering::Relaxed);
        self.used_cache_size.store(used, Ordering::Relaxed);
    }
    
    // 计算平均延迟
    fn calculate_average_latency(latencies: &[Duration]) -> f64 {
        if latencies.is_empty() {
            return 0.0;
        }
        
        let total_ms: f64 = latencies.iter()
            .map(|d| d.as_secs_f64() * 1000.0)
            .sum();
        
        total_ms / latencies.len() as f64
    }
    
    // 生成当前指标快照
    pub fn get_metrics(&self) -> CacheMetrics {
        let cache_hits = self.cache_hits.load(Ordering::Relaxed);
        let cache_misses = self.cache_misses.load(Ordering::Relaxed);
        let total_cache_requests = cache_hits + cache_misses;
        
        let cache_hit_rate = if total_cache_requests > 0 {
            cache_hits as f64 / total_cache_requests as f64
        } else {
            0.0
        };
        
        let total_cache_size = self.total_cache_size.load(Ordering::Relaxed);
        let used_cache_size = self.used_cache_size.load(Ordering::Relaxed);
        let cache_utilization = if total_cache_size > 0 {
            used_cache_size as f64 / total_cache_size as f64
        } else {
            0.0
        };
        
        let read_latencies = self.read_latencies.read();
        let write_latencies = self.write_latencies.read();
        let cache_latencies = self.cache_latencies.read();
        
        CacheMetrics {
            cache_hits,
            cache_misses,
            cache_hit_rate,
            total_reads: self.total_reads.load(Ordering::Relaxed),
            total_writes: self.total_writes.load(Ordering::Relaxed),
            total_evictions: self.total_evictions.load(Ordering::Relaxed),
            total_cache_tasks: self.total_cache_tasks.load(Ordering::Relaxed),
            active_cache_tasks: self.active_cache_tasks.load(Ordering::Relaxed),
            avg_read_latency_ms: Self::calculate_average_latency(&read_latencies),
            avg_write_latency_ms: Self::calculate_average_latency(&write_latencies),
            avg_cache_latency_ms: Self::calculate_average_latency(&cache_latencies),
            total_cache_size,
            used_cache_size,
            cache_utilization,
            available_cache_size: total_cache_size.saturating_sub(used_cache_size),
            cached_files_count: self.cached_files_count.load(Ordering::Relaxed),
            caching_files_count: self.caching_files_count.load(Ordering::Relaxed),
            failed_files_count: self.failed_files_count.load(Ordering::Relaxed),
            nfs_bytes_read: self.nfs_bytes_read.load(Ordering::Relaxed),
            nfs_bytes_written: self.nfs_bytes_written.load(Ordering::Relaxed),
            nfs_operations: self.nfs_operations.load(Ordering::Relaxed),
            cache_errors: self.cache_errors.load(Ordering::Relaxed),
            nfs_errors: self.nfs_errors.load(Ordering::Relaxed),
            checksum_errors: self.checksum_errors.load(Ordering::Relaxed),
            last_updated: SystemTime::now(),
            uptime_seconds: self.start_time.elapsed().as_secs(),
        }
    }
    
    // 保存历史指标
    pub fn save_snapshot(&self) {
        let metrics = self.get_metrics();
        let mut history = self.historical_metrics.write();
        
        history.push(metrics);
        
        // 保持历史记录在限制范围内
        if history.len() > self.max_history_size {
            history.remove(0);
        }
    }
    
    // 获取历史指标
    pub fn get_historical_metrics(&self) -> Vec<CacheMetrics> {
        self.historical_metrics.read().clone()
    }
    
    // 清理延迟统计（避免内存泄漏）
    pub fn cleanup_latency_stats(&self) {
        const MAX_LATENCY_SAMPLES: usize = 10000;
        
        let mut read_latencies = self.read_latencies.write();
        if read_latencies.len() > MAX_LATENCY_SAMPLES {
            let excess = read_latencies.len() - MAX_LATENCY_SAMPLES;
            read_latencies.drain(0..excess);
        }
        
        let mut write_latencies = self.write_latencies.write();
        if write_latencies.len() > MAX_LATENCY_SAMPLES {
            let excess = write_latencies.len() - MAX_LATENCY_SAMPLES;
            write_latencies.drain(0..excess);
        }
        
        let mut cache_latencies = self.cache_latencies.write();
        if cache_latencies.len() > MAX_LATENCY_SAMPLES {
            let excess = cache_latencies.len() - MAX_LATENCY_SAMPLES;
            cache_latencies.drain(0..excess);
        }
    }
    
    // 重置统计
    pub fn reset_stats(&self) {
        self.cache_hits.store(0, Ordering::Relaxed);
        self.cache_misses.store(0, Ordering::Relaxed);
        self.total_reads.store(0, Ordering::Relaxed);
        self.total_writes.store(0, Ordering::Relaxed);
        self.total_evictions.store(0, Ordering::Relaxed);
        self.total_cache_tasks.store(0, Ordering::Relaxed);
        self.nfs_bytes_read.store(0, Ordering::Relaxed);
        self.nfs_bytes_written.store(0, Ordering::Relaxed);
        self.nfs_operations.store(0, Ordering::Relaxed);
        self.cache_errors.store(0, Ordering::Relaxed);
        self.nfs_errors.store(0, Ordering::Relaxed);
        self.checksum_errors.store(0, Ordering::Relaxed);
        
        self.read_latencies.write().clear();
        self.write_latencies.write().clear();
        self.cache_latencies.write().clear();
        self.historical_metrics.write().clear();
    }
}

// 性能监控任务
pub struct PerformanceMonitor {
    metrics: Arc<MetricsCollector>,
    monitoring_interval: Duration,
}

impl PerformanceMonitor {
    pub fn new(metrics: Arc<MetricsCollector>, interval: Duration) -> Self {
        Self {
            metrics,
            monitoring_interval: interval,
        }
    }
    
    pub async fn start_monitoring(&self) {
        let mut interval = tokio::time::interval(self.monitoring_interval);
        
        loop {
            interval.tick().await;
            
            // 保存快照
            self.metrics.save_snapshot();
            
            // 清理延迟统计
            self.metrics.cleanup_latency_stats();
            
            // 记录当前指标
            let metrics = self.metrics.get_metrics();
            tracing::info!(
                "Cache metrics: hit_rate={:.2}%, utilization={:.2}%, active_tasks={}, errors={}",
                metrics.cache_hit_rate * 100.0,
                metrics.cache_utilization * 100.0,
                metrics.active_cache_tasks,
                metrics.cache_errors + metrics.nfs_errors + metrics.checksum_errors
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    
    #[test]
    fn test_metrics_collection() {
        let collector = MetricsCollector::new();
        
        // 记录一些操作
        collector.record_cache_hit();
        collector.record_cache_hit();
        collector.record_cache_miss();
        collector.record_read(Duration::from_millis(10));
        collector.record_write(Duration::from_millis(20));
        
        let metrics = collector.get_metrics();
        
        assert_eq!(metrics.cache_hits, 2);
        assert_eq!(metrics.cache_misses, 1);
        assert_eq!(metrics.total_reads, 1);
        assert_eq!(metrics.total_writes, 1);
        assert!((metrics.cache_hit_rate - 0.666666).abs() < 0.001);
        assert!(metrics.avg_read_latency_ms > 0.0);
        assert!(metrics.avg_write_latency_ms > 0.0);
    }
    
    #[test]
    fn test_cache_utilization() {
        let collector = MetricsCollector::new();
        
        collector.update_cache_size(1000, 600);
        let metrics = collector.get_metrics();
        
        assert_eq!(metrics.total_cache_size, 1000);
        assert_eq!(metrics.used_cache_size, 600);
        assert_eq!(metrics.available_cache_size, 400);
        assert!((metrics.cache_utilization - 0.6).abs() < 0.001);
    }
    
    #[test]
    fn test_historical_metrics() {
        let collector = MetricsCollector::new();
        
        // 保存几个快照
        collector.record_cache_hit();
        collector.save_snapshot();
        
        collector.record_cache_hit();
        collector.save_snapshot();
        
        let history = collector.get_historical_metrics();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].cache_hits, 1);
        assert_eq!(history[1].cache_hits, 2);
    }
    
    #[test]
    fn test_reset_stats() {
        let collector = MetricsCollector::new();
        
        collector.record_cache_hit();
        collector.record_cache_miss();
        collector.record_read(Duration::from_millis(10));
        
        let metrics_before = collector.get_metrics();
        assert!(metrics_before.cache_hits > 0);
        assert!(metrics_before.cache_misses > 0);
        assert!(metrics_before.total_reads > 0);
        
        collector.reset_stats();
        
        let metrics_after = collector.get_metrics();
        assert_eq!(metrics_after.cache_hits, 0);
        assert_eq!(metrics_after.cache_misses, 0);
        assert_eq!(metrics_after.total_reads, 0);
    }
} 