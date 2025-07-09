# NFS-CacheFS 测试计划

本文档定义了NFS-CacheFS的全面测试策略，包括功能测试、性能测试、压力测试和故障恢复测试。

## 测试环境要求

### 硬件环境
- CPU: 至少4核
- 内存: 至少16GB
- NVMe SSD: 至少100GB可用空间
- 网络: 千兆以太网或更高

### 软件环境
- Linux Kernel 5.4+
- FUSE 3.0+
- Rust 1.75+
- Docker（用于隔离测试）

### 测试数据集
```bash
# 创建测试数据
scripts/generate_test_data.sh
```

测试数据包括：
- 小文件集：10,000个1KB-1MB文件
- 中等文件集：1,000个1MB-100MB文件  
- 大文件集：10个1GB-10GB文件
- 混合负载：各种大小的文件混合

## 功能测试

### 1. 基础文件操作测试

#### 1.1 文件读取测试
```rust
#[test]
fn test_file_read_passthrough() {
    // 测试首次读取时的NFS穿透
    let content = read_file("/mnt/cached/test.txt");
    assert_eq!(content, expected_content);
}

#[test]
fn test_file_read_from_cache() {
    // 测试从缓存读取
    read_file("/mnt/cached/test.txt"); // 触发缓存
    wait_for_cache_completion();
    let content = read_file("/mnt/cached/test.txt");
    assert_cache_hit();
}
```

#### 1.2 目录操作测试
```rust
#[test]
fn test_directory_listing() {
    // 测试目录列表
    let entries = list_directory("/mnt/cached/");
    assert!(entries.contains("file1.txt"));
}

#[test]
fn test_recursive_directory_walk() {
    // 测试递归目录遍历
    let all_files = walk_directory("/mnt/cached/");
    assert_eq!(all_files.len(), expected_count);
}
```

#### 1.3 文件属性测试
```rust
#[test]
fn test_file_attributes() {
    // 测试文件属性保持
    let nfs_stat = stat("/mnt/nfs/file.txt");
    let cache_stat = stat("/mnt/cached/file.txt");
    assert_eq!(nfs_stat.size, cache_stat.size);
    assert_eq!(nfs_stat.mode, cache_stat.mode);
}
```

### 2. 缓存行为测试

#### 2.1 异步缓存测试
```rust
#[test]
fn test_async_caching_behavior() {
    // 验证异步缓存不阻塞读取
    let start = Instant::now();
    let handle = open("/mnt/cached/large_file.bin");
    let first_read_time = start.elapsed();
    
    // 首次读取应该很快返回
    assert!(first_read_time < Duration::from_millis(100));
    
    // 验证后台缓存正在进行
    assert!(is_caching_in_progress("/mnt/cached/large_file.bin"));
}
```

#### 2.2 缓存驱逐测试
```rust
#[test] 
fn test_lru_eviction() {
    // 填满缓存
    fill_cache_to_capacity();
    
    // 访问新文件触发驱逐
    read_file("/mnt/cached/new_large_file.bin");
    
    // 验证最少使用的文件被驱逐
    assert!(!is_cached("/mnt/cached/least_used_file.bin"));
    assert!(is_cached("/mnt/cached/new_large_file.bin"));
}
```

#### 2.3 缓存一致性测试
```rust
#[test]
fn test_cache_consistency() {
    // 读取文件建立缓存
    let original = read_file("/mnt/cached/test.txt");
    
    // 直接修改NFS上的文件
    write_file("/mnt/nfs/test.txt", "new content");
    
    // 验证缓存失效机制
    let updated = read_file("/mnt/cached/test.txt");
    assert_eq!(updated, "new content");
}
```

### 3. 并发测试

#### 3.1 多线程读取测试
```rust
#[test]
fn test_concurrent_reads() {
    let handles: Vec<_> = (0..100).map(|i| {
        thread::spawn(move || {
            read_file(&format!("/mnt/cached/file_{}.txt", i))
        })
    }).collect();
    
    for handle in handles {
        assert!(handle.join().is_ok());
    }
}
```

#### 3.2 并发缓存测试
```rust
#[test]
fn test_concurrent_caching_limit() {
    // 同时触发多个缓存任务
    trigger_multiple_cache_tasks(10);
    
    // 验证并发限制
    let active_tasks = get_active_cache_tasks();
    assert!(active_tasks <= MAX_CONCURRENT_CACHING);
}
```

## 性能测试

### 1. 基准性能测试

#### 1.1 顺序读取性能
```bash
#!/bin/bash
# sequential_read_benchmark.sh

echo "=== NFS直接读取 ==="
dd if=/mnt/nfs/10gb_file.bin of=/dev/null bs=1M status=progress

echo "=== CacheFS首次读取 ==="
drop_caches
dd if=/mnt/cached/10gb_file.bin of=/dev/null bs=1M status=progress

echo "=== CacheFS缓存读取 ==="
dd if=/mnt/cached/10gb_file.bin of=/dev/null bs=1M status=progress
```

#### 1.2 随机访问性能
```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_random_access(c: &mut Criterion) {
    c.bench_function("random_4k_reads", |b| {
        b.iter(|| {
            let offset = rand::random::<u64>() % FILE_SIZE;
            read_at("/mnt/cached/test.bin", offset, 4096)
        });
    });
}
```

#### 1.3 IOPS测试
```bash
# 使用fio测试IOPS
fio --name=random_read \
    --filename=/mnt/cached/test.bin \
    --rw=randread \
    --bs=4k \
    --direct=1 \
    --numjobs=16 \
    --time_based \
    --runtime=60 \
    --group_reporting
```

### 2. 延迟测试

```rust
#[test]
fn measure_cache_impact_on_latency() {
    let mut nfs_latencies = Vec::new();
    let mut cache_latencies = Vec::new();
    
    // 测量NFS延迟
    for _ in 0..1000 {
        let start = Instant::now();
        read_block("/mnt/nfs/test.bin", 0, 4096);
        nfs_latencies.push(start.elapsed());
    }
    
    // 测量缓存延迟
    for _ in 0..1000 {
        let start = Instant::now();
        read_block("/mnt/cached/test.bin", 0, 4096);
        cache_latencies.push(start.elapsed());
    }
    
    // 计算P50, P95, P99
    println!("NFS P99: {:?}", percentile(&nfs_latencies, 99));
    println!("Cache P99: {:?}", percentile(&cache_latencies, 99));
}
```

## 压力测试

### 1. 缓存抖动测试

```bash
#!/bin/bash
# cache_thrashing_test.sh

# 创建超过缓存容量的文件集
total_size=$((CACHE_SIZE * 2))

# 循环访问所有文件
for i in {1..10}; do
    echo "=== 迭代 $i ==="
    for file in /mnt/cached/large_files/*; do
        dd if=$file of=/dev/null bs=1M count=100 2>/dev/null
    done
    
    # 检查缓存命中率
    check_cache_hit_rate
done
```

### 2. 内存压力测试

```rust
#[test]
fn test_memory_usage_under_load() {
    let initial_memory = get_process_memory();
    
    // 打开大量文件
    let handles: Vec<_> = (0..10000).map(|i| {
        open(&format!("/mnt/cached/file_{}.txt", i))
    }).collect();
    
    let peak_memory = get_process_memory();
    
    // 验证内存使用在合理范围内
    assert!(peak_memory - initial_memory < 1_000_000_000); // 1GB
}
```

### 3. 长时间运行测试

```bash
#!/bin/bash
# endurance_test.sh

# 运行72小时压力测试
start_time=$(date +%s)
end_time=$((start_time + 259200)) # 72小时

while [ $(date +%s) -lt $end_time ]; do
    # 混合工作负载
    parallel -j 20 ::: \
        "dd if=/mnt/cached/file1.bin of=/dev/null bs=1M" \
        "find /mnt/cached -type f | head -1000" \
        "ls -la /mnt/cached/" \
        "cat /mnt/cached/small_file.txt"
    
    # 每小时检查一次健康状态
    if [ $(($(date +%s) % 3600)) -eq 0 ]; then
        check_system_health
    fi
done
```

## 故障恢复测试

### 1. 崩溃恢复测试

```rust
#[test]
fn test_crash_recovery() {
    // 触发缓存操作
    trigger_large_file_caching();
    
    // 模拟崩溃
    kill_process("nfs-cachefs");
    
    // 重启服务
    start_service("nfs-cachefs");
    
    // 验证缓存状态恢复
    assert!(verify_cache_integrity());
    
    // 验证可以继续正常工作
    let content = read_file("/mnt/cached/test.txt");
    assert!(content.is_ok());
}
```

### 2. 网络故障测试

```bash
#!/bin/bash
# network_failure_test.sh

# 模拟网络中断
iptables -A INPUT -s $NFS_SERVER_IP -j DROP
iptables -A OUTPUT -d $NFS_SERVER_IP -j DROP

# 验证缓存的文件仍可访问
dd if=/mnt/cached/cached_file.bin of=/dev/null bs=1M

# 验证新文件访问的错误处理
if dd if=/mnt/cached/new_file.bin of=/dev/null bs=1M 2>/dev/null; then
    echo "错误：应该失败但成功了"
    exit 1
fi

# 恢复网络
iptables -D INPUT -s $NFS_SERVER_IP -j DROP
iptables -D OUTPUT -d $NFS_SERVER_IP -j DROP
```

### 3. 磁盘满测试

```rust
#[test]
fn test_cache_disk_full() {
    // 填满缓存磁盘
    fill_disk("/mnt/nvme", 95);
    
    // 尝试缓存新文件
    let result = read_file("/mnt/cached/new_large_file.bin");
    
    // 验证优雅降级到NFS
    assert!(result.is_ok());
    assert!(!is_cached("/mnt/cached/new_large_file.bin"));
}
```

## 集成测试

### 1. PyTorch工作负载模拟

```python
# pytorch_workload_test.py
import torch
import time
import os

def test_model_loading():
    """模拟PyTorch模型加载场景"""
    model_path = "/mnt/cached/models/large_model.pth"
    
    # 首次加载（触发缓存）
    start = time.time()
    model1 = torch.load(model_path)
    first_load_time = time.time() - start
    
    del model1
    torch.cuda.empty_cache()
    
    # 等待缓存完成
    time.sleep(10)
    
    # 第二次加载（从缓存）
    start = time.time()
    model2 = torch.load(model_path)
    cached_load_time = time.time() - start
    
    print(f"首次加载: {first_load_time:.2f}s")
    print(f"缓存加载: {cached_load_time:.2f}s")
    print(f"加速比: {first_load_time/cached_load_time:.2f}x")
    
    assert cached_load_time < first_load_time * 0.2
```

### 2. 真实数据集测试

```bash
#!/bin/bash
# real_dataset_test.sh

# 测试ImageNet数据集访问
echo "=== 测试ImageNet数据集加载 ==="
python3 -c "
from torchvision import datasets
import time

start = time.time()
dataset = datasets.ImageFolder('/mnt/cached/imagenet/train')
print(f'数据集加载时间: {time.time() - start:.2f}s')
print(f'样本数: {len(dataset)}')
"

# 测试随机访问性能
echo "=== 测试随机批次加载 ==="
python3 benchmark_dataloader.py --data-path /mnt/cached/imagenet
```

## 测试自动化

### Jenkins Pipeline配置

```groovy
pipeline {
    agent any
    
    stages {
        stage('单元测试') {
            steps {
                sh 'cargo test --lib'
            }
        }
        
        stage('集成测试') {
            steps {
                sh 'cargo test --test integration'
            }
        }
        
        stage('性能测试') {
            steps {
                sh 'cargo bench'
                sh './scripts/performance_tests.sh'
            }
        }
        
        stage('压力测试') {
            when {
                branch 'main'
            }
            steps {
                sh './scripts/stress_tests.sh'
            }
        }
    }
    
    post {
        always {
            junit 'target/test-results/*.xml'
            archiveArtifacts 'target/benchmark-results/*'
        }
    }
}
```

## 测试报告模板

### 性能测试报告示例

```markdown
# NFS-CacheFS 性能测试报告

**测试日期**: 2024-01-20
**版本**: v1.0.0
**测试环境**: 
- CPU: Intel Xeon Gold 6248 @ 2.50GHz
- 内存: 128GB DDR4
- NVMe: Samsung 980 PRO 2TB
- 网络: 10Gbps

## 测试结果摘要

| 指标 | NFS直连 | CacheFS(首次) | CacheFS(缓存) | 提升 |
|------|---------|---------------|---------------|------|
| 10GB顺序读 | 100s | 102s | 8s | 12.5x |
| 4K随机读IOPS | 5,000 | 4,800 | 250,000 | 50x |
| 平均延迟 | 10ms | 10.5ms | 0.04ms | 250x |
| 缓存命中率 | N/A | 0% | 95% | - |

## 详细测试数据
[附加详细图表和数据]
```

## 测试覆盖率目标

- 单元测试覆盖率: ≥ 80%
- 集成测试覆盖率: ≥ 70%
- 关键路径覆盖率: 100%

使用以下命令生成覆盖率报告：
```bash
cargo tarpaulin --out Html --output-dir target/coverage
``` 