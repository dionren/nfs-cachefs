use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nfs_cachefs::cache::state::{CacheEntry, CachePriority};
use nfs_cachefs::cache::eviction::LruEvictionPolicy;
use nfs_cachefs::cache::metrics::MetricsCollector;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

fn benchmark_cache_entry_operations(c: &mut Criterion) {
    c.bench_function("cache_entry_new", |b| {
        b.iter(|| {
            let entry = CacheEntry::new(black_box(1024));
            black_box(entry);
        });
    });
    
    c.bench_function("cache_entry_checksum", |b| {
        let data = vec![0u8; 1024];
        b.iter(|| {
            let checksum = CacheEntry::calculate_checksum(black_box(&data));
            black_box(checksum);
        });
    });
    
    c.bench_function("cache_entry_lru_score", |b| {
        let mut entry = CacheEntry::new(1024);
        entry.complete_caching(1024, None);
        entry.mark_accessed();
        
        b.iter(|| {
            let score = entry.calculate_lru_score();
            black_box(score);
        });
    });
}

fn benchmark_eviction_policy(c: &mut Criterion) {
    c.bench_function("lru_eviction_select_victims", |b| {
        let mut policy = LruEvictionPolicy::new(1000);
        let mut entries = HashMap::new();
        
        // 创建测试数据
        for i in 0..100 {
            let path = PathBuf::from(format!("/cache/file_{}.txt", i));
            let mut entry = CacheEntry::new(1024);
            entry.complete_caching(1024, None);
            
            policy.on_insert(path.clone(), &entry);
            entries.insert(path, entry);
        }
        
        b.iter(|| {
            let victims = policy.select_victims(black_box(&entries), black_box(10240));
            black_box(victims);
        });
    });
}

fn benchmark_metrics_collection(c: &mut Criterion) {
    c.bench_function("metrics_record_operations", |b| {
        let collector = MetricsCollector::new();
        
        b.iter(|| {
            collector.record_cache_hit();
            collector.record_cache_miss();
            collector.record_read(Duration::from_millis(10));
            collector.record_write(Duration::from_millis(20));
            collector.record_nfs_read(1024);
        });
    });
    
    c.bench_function("metrics_get_snapshot", |b| {
        let collector = MetricsCollector::new();
        
        // 添加一些数据
        for _ in 0..100 {
            collector.record_cache_hit();
            collector.record_read(Duration::from_millis(10));
        }
        
        b.iter(|| {
            let metrics = collector.get_metrics();
            black_box(metrics);
        });
    });
}

criterion_group!(
    benches,
    benchmark_cache_entry_operations,
    benchmark_eviction_policy,
    benchmark_metrics_collection
);
criterion_main!(benches); 