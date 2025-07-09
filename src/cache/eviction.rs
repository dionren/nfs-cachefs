use std::collections::HashMap;
use std::path::PathBuf;
use lru::LruCache;
use crate::cache::state::{CacheEntry, CachePriority};


pub trait EvictionPolicy: Send + Sync {
    fn should_evict(&self, current_size: u64, max_size: u64) -> bool;
    fn select_victims(&self, entries: &HashMap<PathBuf, CacheEntry>, needed_space: u64) -> Vec<PathBuf>;
    fn on_access(&mut self, path: &PathBuf, entry: &CacheEntry);
    fn on_insert(&mut self, path: PathBuf, entry: &CacheEntry);
    fn on_remove(&mut self, path: &PathBuf);
}

pub struct LruEvictionPolicy {
    access_order: LruCache<PathBuf, ()>,
    protected_paths: Vec<PathBuf>,
}

impl LruEvictionPolicy {
    pub fn new(capacity: usize) -> Self {
        Self {
            access_order: LruCache::new(capacity.try_into().unwrap()),
            protected_paths: Vec::new(),
        }
    }
    
    pub fn protect_path(&mut self, path: PathBuf) {
        self.protected_paths.push(path);
    }
    
    pub fn unprotect_path(&mut self, path: &PathBuf) {
        self.protected_paths.retain(|p| p != path);
    }
    
    fn is_protected(&self, path: &PathBuf) -> bool {
        self.protected_paths.contains(path)
    }
}

impl EvictionPolicy for LruEvictionPolicy {
    fn should_evict(&self, current_size: u64, max_size: u64) -> bool {
        current_size > max_size
    }
    
    fn select_victims(&self, entries: &HashMap<PathBuf, CacheEntry>, needed_space: u64) -> Vec<PathBuf> {
        let mut candidates: Vec<_> = entries.iter()
            .filter(|(path, entry)| {
                // 不驱逐正在缓存中的文件、受保护的文件和关键优先级的文件
                !entry.status.is_caching() 
                && !self.is_protected(path) 
                && entry.priority != CachePriority::Critical
            })
            .collect();
        
        // 按照 LRU 分数排序（分数越高越应该被驱逐）
        candidates.sort_by(|(_, a), (_, b)| {
            b.calculate_lru_score().partial_cmp(&a.calculate_lru_score()).unwrap()
        });
        
        let mut victims = Vec::new();
        let mut freed_space = 0u64;
        
        for (path, entry) in candidates {
            if freed_space >= needed_space {
                break;
            }
            
            victims.push((*path).clone());
            freed_space += entry.size;
        }
        
        victims
    }
    
    fn on_access(&mut self, path: &PathBuf, _entry: &CacheEntry) {
        self.access_order.put(path.clone(), ());
    }
    
    fn on_insert(&mut self, path: PathBuf, _entry: &CacheEntry) {
        self.access_order.put(path, ());
    }
    
    fn on_remove(&mut self, path: &PathBuf) {
        self.access_order.pop(path);
    }
}

pub struct LfuEvictionPolicy {
    access_counts: HashMap<PathBuf, u64>,
    protected_paths: Vec<PathBuf>,
}

impl LfuEvictionPolicy {
    pub fn new() -> Self {
        Self {
            access_counts: HashMap::new(),
            protected_paths: Vec::new(),
        }
    }
    
    pub fn protect_path(&mut self, path: PathBuf) {
        self.protected_paths.push(path);
    }
    
    pub fn unprotect_path(&mut self, path: &PathBuf) {
        self.protected_paths.retain(|p| p != path);
    }
    
    fn is_protected(&self, path: &PathBuf) -> bool {
        self.protected_paths.contains(path)
    }
}

impl EvictionPolicy for LfuEvictionPolicy {
    fn should_evict(&self, current_size: u64, max_size: u64) -> bool {
        current_size > max_size
    }
    
    fn select_victims(&self, entries: &HashMap<PathBuf, CacheEntry>, needed_space: u64) -> Vec<PathBuf> {
        let mut candidates: Vec<_> = entries.iter()
            .filter(|(path, entry)| {
                !entry.status.is_caching() 
                && !self.is_protected(path) 
                && entry.priority != CachePriority::Critical
            })
            .collect();
        
        // 按照访问频率排序（频率越低越应该被驱逐）
        candidates.sort_by(|(path_a, entry_a), (path_b, entry_b)| {
            let freq_a = self.access_counts.get(*path_a).unwrap_or(&0);
            let freq_b = self.access_counts.get(*path_b).unwrap_or(&0);
            
            freq_a.cmp(freq_b)
                .then_with(|| entry_a.priority.cmp(&entry_b.priority))
                .then_with(|| entry_b.get_last_access_seconds().cmp(&entry_a.get_last_access_seconds()))
        });
        
        let mut victims = Vec::new();
        let mut freed_space = 0u64;
        
        for (path, entry) in candidates {
            if freed_space >= needed_space {
                break;
            }
            
            victims.push((*path).clone());
            freed_space += entry.size;
        }
        
        victims
    }
    
    fn on_access(&mut self, path: &PathBuf, _entry: &CacheEntry) {
        *self.access_counts.entry(path.clone()).or_insert(0) += 1;
    }
    
    fn on_insert(&mut self, path: PathBuf, _entry: &CacheEntry) {
        self.access_counts.insert(path, 1);
    }
    
    fn on_remove(&mut self, path: &PathBuf) {
        self.access_counts.remove(path);
    }
}

// ARC (Adaptive Replacement Cache) 策略的简化实现
pub struct ArcEvictionPolicy {
    t1: LruCache<PathBuf, ()>,  // 最近使用的页面
    t2: LruCache<PathBuf, ()>,  // 频繁使用的页面
    b1: LruCache<PathBuf, ()>,  // 最近被驱逐的T1页面
    b2: LruCache<PathBuf, ()>,  // 最近被驱逐的T2页面
    p: usize,                   // 目标T1大小
    protected_paths: Vec<PathBuf>,
}

impl ArcEvictionPolicy {
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.try_into().unwrap();
        Self {
            t1: LruCache::new(cap),
            t2: LruCache::new(cap),
            b1: LruCache::new(cap),
            b2: LruCache::new(cap),
            p: 0,
            protected_paths: Vec::new(),
        }
    }
    
    pub fn protect_path(&mut self, path: PathBuf) {
        self.protected_paths.push(path);
    }
    
    pub fn unprotect_path(&mut self, path: &PathBuf) {
        self.protected_paths.retain(|p| p != path);
    }
    
    fn is_protected(&self, path: &PathBuf) -> bool {
        self.protected_paths.contains(path)
    }
}

impl EvictionPolicy for ArcEvictionPolicy {
    fn should_evict(&self, current_size: u64, max_size: u64) -> bool {
        current_size > max_size
    }
    
    fn select_victims(&self, entries: &HashMap<PathBuf, CacheEntry>, needed_space: u64) -> Vec<PathBuf> {
        let mut candidates: Vec<_> = entries.iter()
            .filter(|(path, entry)| {
                !entry.status.is_caching() 
                && !self.is_protected(path) 
                && entry.priority != CachePriority::Critical
            })
            .collect();
        
        // 简化的ARC策略：优先驱逐T1中的页面
        candidates.sort_by(|(path_a, entry_a), (path_b, entry_b)| {
            let in_t1_a = self.t1.contains(*path_a);
            let in_t1_b = self.t1.contains(*path_b);
            
            match (in_t1_a, in_t1_b) {
                (true, false) => std::cmp::Ordering::Less,  // T1中的页面优先被驱逐
                (false, true) => std::cmp::Ordering::Greater,
                _ => entry_a.calculate_lru_score().partial_cmp(&entry_b.calculate_lru_score()).unwrap(),
            }
        });
        
        let mut victims = Vec::new();
        let mut freed_space = 0u64;
        
        for (path, entry) in candidates {
            if freed_space >= needed_space {
                break;
            }
            
            victims.push((*path).clone());
            freed_space += entry.size;
        }
        
        victims
    }
    
    fn on_access(&mut self, path: &PathBuf, _entry: &CacheEntry) {
        if self.t1.contains(path) {
            self.t1.pop(path);
            self.t2.put(path.clone(), ());
        } else if self.t2.contains(path) {
            self.t2.get(path);  // 更新访问顺序
        } else if self.b1.contains(path) {
            self.b1.pop(path);
            self.t2.put(path.clone(), ());
            self.p = std::cmp::min(self.p + 1, self.t1.cap().get());
        } else if self.b2.contains(path) {
            self.b2.pop(path);
            self.t2.put(path.clone(), ());
            self.p = self.p.saturating_sub(1);
        }
    }
    
    fn on_insert(&mut self, path: PathBuf, _entry: &CacheEntry) {
        self.t1.put(path, ());
    }
    
    fn on_remove(&mut self, path: &PathBuf) {
        if self.t1.pop(path).is_some() {
            self.b1.put(path.clone(), ());
        } else if self.t2.pop(path).is_some() {
            self.b2.put(path.clone(), ());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::state::{CacheStatus, CacheEntry};
    use std::time::SystemTime;
    
    #[test]
    fn test_lru_eviction() {
        let mut policy = LruEvictionPolicy::new(100);
        let mut entries = HashMap::new();
        
        // 添加一些缓存条目
        let path1 = PathBuf::from("/cache/file1.txt");
        let path2 = PathBuf::from("/cache/file2.txt");
        let path3 = PathBuf::from("/cache/file3.txt");
        
        let mut entry1 = CacheEntry::new(1000);
        entry1.complete_caching(1000, None);
        let mut entry2 = CacheEntry::new(2000);
        entry2.complete_caching(2000, None);
        let mut entry3 = CacheEntry::new(3000);
        entry3.complete_caching(3000, None);
        
        entries.insert(path1.clone(), entry1.clone());
        entries.insert(path2.clone(), entry2.clone());
        entries.insert(path3.clone(), entry3.clone());
        
        policy.on_insert(path1.clone(), &entry1);
        policy.on_insert(path2.clone(), &entry2);
        policy.on_insert(path3.clone(), &entry3);
        
        // 访问 file1，使其成为最近使用的
        entry1.mark_accessed();  // 更新 entry1 的访问时间
        entries.insert(path1.clone(), entry1.clone());  // 更新 entries 中的条目
        policy.on_access(&path1, &entry1);
        
        // 需要释放 3000 字节的空间
        let victims = policy.select_victims(&entries, 3000);
        
        // 验证 file1 没有被选中（因为它被最近访问）
        assert!(!victims.contains(&path1));
        
        // 验证至少选择了足够的文件来满足空间需求
        let total_freed: u64 = victims.iter()
            .map(|path| entries.get(path).unwrap().size)
            .sum();
        assert!(total_freed >= 3000);
        
        // 测试需要更多空间的情况
        let victims_large = policy.select_victims(&entries, 4500);
        assert!(!victims_large.contains(&path1)); // file1 仍然被保护
        
        let total_freed_large: u64 = victims_large.iter()
            .map(|path| entries.get(path).unwrap().size)
            .sum();
        assert!(total_freed_large >= 4500);
    }
    
    #[test]
    fn test_protected_paths() {
        let mut policy = LruEvictionPolicy::new(100);
        let mut entries = HashMap::new();
        
        let path1 = PathBuf::from("/cache/file1.txt");
        let path2 = PathBuf::from("/cache/file2.txt");
        
        let mut entry1 = CacheEntry::new(1000);
        entry1.complete_caching(1000, None);
        let mut entry2 = CacheEntry::new(2000);
        entry2.complete_caching(2000, None);
        
        entries.insert(path1.clone(), entry1.clone());
        entries.insert(path2.clone(), entry2.clone());
        
        policy.on_insert(path1.clone(), &entry1);
        policy.on_insert(path2.clone(), &entry2);
        
        // 保护 file1
        policy.protect_path(path1.clone());
        
        let victims = policy.select_victims(&entries, 1500);
        
        // 只有 file2 应该被选中（file1 被保护）
        assert_eq!(victims.len(), 1);
        assert!(victims.contains(&path2));
        assert!(!victims.contains(&path1));
    }
    
    #[test]
    fn test_critical_priority_protection() {
        let mut policy = LruEvictionPolicy::new(100);
        let mut entries = HashMap::new();
        
        let path1 = PathBuf::from("/cache/file1.txt");
        let path2 = PathBuf::from("/cache/file2.txt");
        
        let mut entry1 = CacheEntry::new(1000).with_priority(CachePriority::Critical);
        entry1.complete_caching(1000, None);
        let mut entry2 = CacheEntry::new(2000);
        entry2.complete_caching(2000, None);
        
        entries.insert(path1.clone(), entry1.clone());
        entries.insert(path2.clone(), entry2.clone());
        
        policy.on_insert(path1.clone(), &entry1);
        policy.on_insert(path2.clone(), &entry2);
        
        let victims = policy.select_victims(&entries, 1500);
        
        // 只有 file2 应该被选中（file1 优先级为 Critical）
        assert_eq!(victims.len(), 1);
        assert!(victims.contains(&path2));
        assert!(!victims.contains(&path1));
    }
} 