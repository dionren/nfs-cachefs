use std::path::PathBuf;
use std::time::SystemTime;
use serde::{Deserialize, Serialize};
use crate::cache::state::CachePriority;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTask {
    pub id: String,
    pub source_path: PathBuf,
    pub cache_path: PathBuf,
    pub priority: CachePriority,
    pub retry_count: u32,
    pub max_retries: u32,
    pub created_at: SystemTime,
    pub file_size: Option<u64>,
    pub enable_checksum: bool,
}

impl CacheTask {
    pub fn new(source_path: PathBuf, cache_path: PathBuf) -> Self {
        let id = Self::generate_task_id(&source_path);
        Self {
            id,
            source_path,
            cache_path,
            priority: CachePriority::Normal,
            retry_count: 0,
            max_retries: 3,
            created_at: SystemTime::now(),
            file_size: None,
            enable_checksum: false,
        }
    }
    
    pub fn with_priority(mut self, priority: CachePriority) -> Self {
        self.priority = priority;
        self
    }
    
    pub fn with_checksum(mut self, enable: bool) -> Self {
        self.enable_checksum = enable;
        self
    }
    
    pub fn with_file_size(mut self, size: u64) -> Self {
        self.file_size = Some(size);
        self
    }
    
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }
    
    pub fn can_retry(&self) -> bool {
        self.retry_count < self.max_retries
    }
    
    pub fn increment_retry(&mut self) {
        self.retry_count += 1;
    }
    
    pub fn get_temp_path(&self) -> PathBuf {
        self.cache_path.with_extension(format!("caching.{}", self.retry_count))
    }
    
    fn generate_task_id(path: &PathBuf) -> String {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default().as_nanos().to_be_bytes());
        format!("{:x}", hasher.finalize())[0..16].to_string()
    }
}

impl PartialEq for CacheTask {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for CacheTask {}

impl PartialOrd for CacheTask {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CacheTask {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 优先级高的任务排在前面
        other.priority.cmp(&self.priority)
            .then_with(|| self.created_at.cmp(&other.created_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    
    #[test]
    fn test_task_creation() {
        let source = PathBuf::from("/nfs/test.txt");
        let cache = PathBuf::from("/cache/test.txt");
        let task = CacheTask::new(source.clone(), cache.clone());
        
        assert_eq!(task.source_path, source);
        assert_eq!(task.cache_path, cache);
        assert_eq!(task.priority, CachePriority::Normal);
        assert_eq!(task.retry_count, 0);
        assert!(task.can_retry());
    }
    
    #[test]
    fn test_task_ordering() {
        let source = PathBuf::from("/nfs/test.txt");
        let cache = PathBuf::from("/cache/test.txt");
        
        let normal_task = CacheTask::new(source.clone(), cache.clone());
        let high_task = CacheTask::new(source.clone(), cache.clone())
            .with_priority(CachePriority::High);
        
        assert!(high_task < normal_task);
    }
    
    #[test]
    fn test_retry_mechanism() {
        let source = PathBuf::from("/nfs/test.txt");
        let cache = PathBuf::from("/cache/test.txt");
        let mut task = CacheTask::new(source, cache).with_max_retries(2);
        
        assert!(task.can_retry());
        task.increment_retry();
        assert!(task.can_retry());
        task.increment_retry();
        assert!(!task.can_retry());
    }
    
    #[test]
    fn test_temp_path_generation() {
        let source = PathBuf::from("/nfs/test.txt");
        let cache = PathBuf::from("/cache/test.txt");
        let mut task = CacheTask::new(source, cache);
        
        let temp1 = task.get_temp_path();
        task.increment_retry();
        let temp2 = task.get_temp_path();
        
        assert_ne!(temp1, temp2);
        assert!(temp1.to_string_lossy().contains("caching.0"));
        assert!(temp2.to_string_lossy().contains("caching.1"));
    }
} 