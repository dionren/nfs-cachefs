use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};

#[derive(Debug, Clone)]
pub enum CacheStatus {
    NotCached,
    CachingInProgress {
        started_at: SystemTime,
        progress: Arc<AtomicU64>,
        total_size: u64,
    },
    Cached {
        cached_at: SystemTime,
        last_accessed: SystemTime,
        file_size: u64,
    },
    Failed {
        failed_at: SystemTime,
        error_message: String,
        retry_count: u32,
    },
}

impl Default for CacheStatus {
    fn default() -> Self {
        Self::NotCached
    }
}

impl CacheStatus {
    pub fn is_cached(&self) -> bool {
        matches!(self, Self::Cached { .. })
    }
    
    pub fn is_caching(&self) -> bool {
        matches!(self, Self::CachingInProgress { .. })
    }
    
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
    
    pub fn get_progress_percentage(&self) -> Option<f64> {
        match self {
            Self::CachingInProgress { progress, total_size, .. } => {
                let current = progress.load(Ordering::Relaxed);
                if *total_size > 0 {
                    Some((current as f64 / *total_size as f64) * 100.0)
                } else {
                    Some(0.0)
                }
            }
            Self::Cached { .. } => Some(100.0),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub size: u64,
    pub status: CacheStatus,
    pub access_count: u64,
    pub checksum: Option<String>,
    pub created_at: SystemTime,
    pub last_modified: SystemTime,
    pub priority: CachePriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CachePriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

impl Default for CachePriority {
    fn default() -> Self {
        Self::Normal
    }
}

impl CacheEntry {
    pub fn new(size: u64) -> Self {
        let now = SystemTime::now();
        Self {
            size,
            status: CacheStatus::NotCached,
            access_count: 0,
            checksum: None,
            created_at: now,
            last_modified: now,
            priority: CachePriority::Normal,
        }
    }
    
    pub fn with_priority(mut self, priority: CachePriority) -> Self {
        self.priority = priority;
        self
    }
    
    pub fn start_caching(&mut self, total_size: u64) -> Arc<AtomicU64> {
        let progress = Arc::new(AtomicU64::new(0));
        self.status = CacheStatus::CachingInProgress {
            started_at: SystemTime::now(),
            progress: Arc::clone(&progress),
            total_size,
        };
        progress
    }
    
    pub fn complete_caching(&mut self, file_size: u64, checksum: Option<String>) {
        let now = SystemTime::now();
        self.status = CacheStatus::Cached {
            cached_at: now,
            last_accessed: now,
            file_size,
        };
        self.checksum = checksum;
        self.last_modified = now;
    }
    
    pub fn mark_failed(&mut self, error_message: String, retry_count: u32) {
        self.status = CacheStatus::Failed {
            failed_at: SystemTime::now(),
            error_message,
            retry_count,
        };
        self.last_modified = SystemTime::now();
    }
    
    pub fn mark_accessed(&mut self) {
        self.access_count += 1;
        if let CacheStatus::Cached { last_accessed, .. } = &mut self.status {
            *last_accessed = SystemTime::now();
        }
    }
    
    pub fn calculate_checksum(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }
    
    pub fn verify_checksum(&self, data: &[u8]) -> bool {
        match &self.checksum {
            Some(expected) => {
                let actual = Self::calculate_checksum(data);
                actual == *expected
            }
            None => true, // 如果没有校验和，认为验证通过
        }
    }
    
    /// 获取缓存条目的年龄（秒）
    pub fn get_age_seconds(&self) -> u64 {
        match SystemTime::now().duration_since(self.created_at) {
            Ok(duration) => duration.as_secs(),
            Err(_) => 0,
        }
    }
    
    /// 获取最后访问时间距现在的秒数
    pub fn get_last_access_seconds(&self) -> u64 {
        let last_access = match &self.status {
            CacheStatus::Cached { last_accessed, .. } => *last_accessed,
            _ => self.created_at,
        };
        
        match SystemTime::now().duration_since(last_access) {
            Ok(duration) => duration.as_secs(),
            Err(_) => 0,
        }
    }
    
    /// 计算 LRU 分数（越小越应该被驱逐）
    pub fn calculate_lru_score(&self) -> f64 {
        let age = self.get_age_seconds() as f64;
        let last_access = self.get_last_access_seconds() as f64;
        let access_frequency = self.access_count as f64 / (age + 1.0);
        let priority_weight = self.priority as u8 as f64;
        
        // 综合考虑最后访问时间、访问频率和优先级
        last_access - (access_frequency * 10.0) - (priority_weight * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    
    #[test]
    fn test_cache_entry_creation() {
        let entry = CacheEntry::new(1024);
        assert_eq!(entry.size, 1024);
        assert!(matches!(entry.status, CacheStatus::NotCached));
        assert_eq!(entry.access_count, 0);
        assert_eq!(entry.priority, CachePriority::Normal);
    }
    
    #[test]
    fn test_cache_status_transitions() {
        let mut entry = CacheEntry::new(1024);
        
        // 初始状态
        assert!(matches!(entry.status, CacheStatus::NotCached));
        assert!(!entry.status.is_cached());
        assert!(!entry.status.is_caching());
        
        // 开始缓存
        let progress = entry.start_caching(1024);
        assert!(entry.status.is_caching());
        assert_eq!(entry.status.get_progress_percentage(), Some(0.0));
        
        // 更新进度
        progress.store(512, Ordering::SeqCst);
        assert_eq!(entry.status.get_progress_percentage(), Some(50.0));
        
        // 完成缓存
        entry.complete_caching(1024, Some("checksum".to_string()));
        assert!(entry.status.is_cached());
        assert_eq!(entry.status.get_progress_percentage(), Some(100.0));
    }
    
    #[test]
    fn test_checksum_verification() {
        let data = b"test data";
        let checksum = CacheEntry::calculate_checksum(data);
        
        let mut entry = CacheEntry::new(data.len() as u64);
        entry.complete_caching(data.len() as u64, Some(checksum));
        
        assert!(entry.verify_checksum(data));
        assert!(!entry.verify_checksum(b"different data"));
    }
    
    #[test]
    fn test_lru_score_calculation() {
        let mut entry = CacheEntry::new(1024);
        entry.complete_caching(1024, None);
        
        let initial_score = entry.calculate_lru_score();
        
        // 模拟访问
        entry.mark_accessed();
        thread::sleep(Duration::from_millis(10));
        
        let after_access_score = entry.calculate_lru_score();
        
        // 访问后分数应该更低（更不容易被驱逐）
        assert!(after_access_score < initial_score);
    }
} 