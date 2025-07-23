# FUSE + io_uring 高性能改造方案

## 执行摘要

本方案旨在通过集成 io_uring 技术，彻底改造 NFS-CacheFS 的 I/O 性能，实现接近硬件极限的读取速度。

### 核心收益
- **缓存命中性能**: 100 MB/s → 2-3 GB/s (20-30倍提升)
- **缓存未命中性能**: 60 MB/s → 200-500 MB/s (3-8倍提升)
- **CPU使用率**: 降低 50-70%
- **系统调用开销**: 减少 80%

## 现状分析

### 当前架构问题
1. **同步I/O瓶颈**
   - 每次读取都涉及用户态/内核态切换
   - 无法批量处理请求
   - 缺乏真正的异步I/O能力

2. **内存拷贝过多**
   - FUSE → 用户缓冲区 → 内核缓冲区 → 磁盘
   - 大文件读取时内存带宽成为瓶颈

3. **系统调用开销**
   - 每个读请求至少2次系统调用
   - 高并发时上下文切换严重

### 现有优化措施
- 智能缓存策略 (SmartCacheConfig)
- 零拷贝读取 (O_DIRECT)
- 大块读取 (64MB blocks)
- 同步直读缓存

## io_uring 技术优势

### 核心特性
1. **真正的异步I/O**
   - 提交和完成分离
   - 批量操作支持
   - 无需线程池

2. **零系统调用开销**
   - 共享内存环形缓冲区
   - 内核轮询模式 (SQPOLL)
   - 用户态直接提交

3. **高级特性**
   - 固定缓冲区 (减少映射开销)
   - 链式操作 (读写原子性)
   - 优先级和调度控制

## 改造方案

### 第一阶段：基础设施 (1周)

#### 1.1 添加依赖
```toml
# Cargo.toml
[dependencies]
io-uring = "0.6"
tokio-uring = "0.5"  # 可选，用于与tokio集成

[features]
io_uring = ["io-uring", "tokio-uring"]
```

#### 1.2 创建 io_uring 模块
```rust
// src/io/mod.rs
pub mod uring;

// src/io/uring.rs
use io_uring::{IoUring, opcode, types};
use std::sync::Arc;
use parking_lot::Mutex;

pub struct IoUringExecutor {
    ring: Arc<Mutex<IoUring>>,
    config: IoUringConfig,
    buffer_pool: BufferPool,
    metrics: IoUringMetrics,
}

pub struct IoUringConfig {
    pub queue_depth: u32,
    pub sq_poll: bool,
    pub io_poll: bool,
    pub fixed_buffers: bool,
    pub huge_pages: bool,
}

impl IoUringExecutor {
    pub fn new(config: IoUringConfig) -> Result<Self> {
        let mut builder = IoUring::builder();
        
        if config.sq_poll {
            builder.setup_sqpoll(1000); // 1ms idle time
        }
        
        if config.io_poll {
            builder.setup_iopoll();
        }
        
        let ring = builder
            .queue_depth(config.queue_depth)
            .build()?;
            
        Ok(Self {
            ring: Arc::new(Mutex::new(ring)),
            config,
            buffer_pool: BufferPool::new(config.queue_depth as usize),
            metrics: IoUringMetrics::new(),
        })
    }
    
    pub async fn read_direct(&self, fd: i32, offset: u64, size: u32) -> Result<Vec<u8>> {
        // 实现直接读取
    }
    
    pub async fn read_fixed(&self, fd: i32, offset: u64, buf_index: u16, size: u32) -> Result<()> {
        // 使用固定缓冲区读取
    }
}
```

#### 1.3 缓冲区管理
```rust
// src/io/buffer.rs
pub struct BufferPool {
    buffers: Vec<AlignedBuffer>,
    free_list: Arc<Mutex<Vec<usize>>>,
}

pub struct AlignedBuffer {
    ptr: *mut u8,
    size: usize,
    alignment: usize,
}

impl AlignedBuffer {
    pub fn new(size: usize) -> Self {
        // 分配对齐的内存，支持 O_DIRECT
        let alignment = 512; // 扇区对齐
        let layout = Layout::from_size_align(size, alignment).unwrap();
        let ptr = unsafe { alloc::alloc(layout) };
        
        Self { ptr, size, alignment }
    }
}
```

### 第二阶段：缓存读取优化 (1周)

#### 2.1 修改 CacheFs::read
```rust
// src/fs/cachefs.rs
impl CacheFs {
    fn read(&mut self, ...) {
        // 检查是否启用 io_uring
        if self.config.nvme.use_io_uring {
            self.read_with_uring(ino, offset, size, reply)
        } else {
            self.read_legacy(ino, offset, size, reply)
        }
    }
    
    fn read_with_uring(&mut self, ino: u64, offset: i64, size: u32, reply: ReplyData) {
        let path = match self.inode_manager.get_path(ino) {
            Some(path) => path,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        
        let cache_path = self.get_cache_path(&path);
        
        // 缓存命中 - 使用 io_uring 优化路径
        if cache_path.exists() {
            let uring = self.io_uring_executor.clone();
            
            tokio::spawn(async move {
                match uring.read_direct_to_fuse(cache_path, offset, size).await {
                    Ok(data) => reply.data(&data),
                    Err(e) => reply.error(libc::EIO),
                }
            });
            
            return;
        }
        
        // 缓存未命中 - 降级到原有逻辑
        self.read_legacy(ino, offset, size, reply)
    }
}
```

#### 2.2 实现零拷贝读取
```rust
// src/io/uring.rs
impl IoUringExecutor {
    pub async fn read_direct_to_fuse(
        &self, 
        path: PathBuf, 
        offset: i64, 
        size: u32
    ) -> Result<Vec<u8>> {
        let file = std::fs::File::open(path)?;
        let fd = file.as_raw_fd();
        
        // 获取预分配的缓冲区
        let (buf_index, buffer) = self.buffer_pool.acquire().await?;
        
        // 构建读取操作
        let read_op = opcode::Read::new(
            types::Fd(fd), 
            buffer.as_mut_ptr(), 
            size
        )
        .offset(offset as u64)
        .build();
        
        // 提交并等待完成
        let mut ring = self.ring.lock();
        unsafe {
            ring.submission()
                .push(&read_op)
                .expect("submission queue full");
        }
        
        ring.submit_and_wait(1)?;
        
        // 获取结果
        let cqe = ring.completion().next().unwrap();
        let bytes_read = cqe.result();
        
        if bytes_read < 0 {
            return Err(io::Error::from_raw_os_error(-bytes_read));
        }
        
        // 返回数据
        let data = buffer.to_vec(bytes_read as usize);
        self.buffer_pool.release(buf_index);
        
        Ok(data)
    }
}
```

### 第三阶段：缓存写入优化 (1周)

#### 3.1 链式操作优化
```rust
// src/cache/manager.rs
impl CacheManager {
    async fn copy_file_with_uring(
        &self,
        source: PathBuf,
        dest: PathBuf,
        size: u64,
    ) -> Result<()> {
        let uring = self.io_uring_executor.clone();
        
        // 使用 splice 进行零拷贝
        uring.splice_file(source, dest, size).await?;
        
        Ok(())
    }
}

// src/io/uring.rs
impl IoUringExecutor {
    pub async fn splice_file(
        &self,
        source: PathBuf,
        dest: PathBuf,
        size: u64,
    ) -> Result<()> {
        let src_file = File::open(source)?;
        let dst_file = File::create(dest)?;
        
        let src_fd = src_file.as_raw_fd();
        let dst_fd = dst_file.as_raw_fd();
        
        // 创建管道用于 splice
        let (pipe_r, pipe_w) = pipe2(O_CLOEXEC)?;
        
        let mut offset = 0u64;
        let chunk_size = 1024 * 1024 * 16; // 16MB chunks
        
        while offset < size {
            let to_copy = (size - offset).min(chunk_size);
            
            // 链式操作: read -> pipe -> write
            let splice_in = opcode::Splice::new(
                types::Fd(src_fd),
                offset as i64,
                types::Fd(pipe_w),
                -1,
                to_copy as u32,
            )
            .build();
            
            let splice_out = opcode::Splice::new(
                types::Fd(pipe_r),
                -1,
                types::Fd(dst_fd),
                offset as i64,
                to_copy as u32,
            )
            .build();
            
            // 提交链式操作
            let mut ring = self.ring.lock();
            unsafe {
                ring.submission()
                    .push(&splice_in)
                    .expect("submission queue full");
                ring.submission()
                    .push(&splice_out)
                    .expect("submission queue full");
            }
            
            ring.submit_and_wait(2)?;
            
            // 处理完成事件
            for _ in 0..2 {
                let cqe = ring.completion().next().unwrap();
                if cqe.result() < 0 {
                    return Err(io::Error::from_raw_os_error(-cqe.result()));
                }
            }
            
            offset += to_copy;
        }
        
        Ok(())
    }
}
```

### 第四阶段：高级优化 (2周)

#### 4.1 批量I/O处理
```rust
pub struct BatchIoRequest {
    requests: Vec<IoRequest>,
    completion_handler: Box<dyn Fn(Vec<IoResult>) + Send>,
}

impl IoUringExecutor {
    pub async fn submit_batch(&self, batch: BatchIoRequest) -> Result<()> {
        let mut ring = self.ring.lock();
        
        // 批量提交所有请求
        for req in &batch.requests {
            let sqe = match req {
                IoRequest::Read { fd, offset, size, buf_index } => {
                    opcode::Read::new(
                        types::Fd(*fd),
                        self.buffer_pool.get_buffer(*buf_index).as_mut_ptr(),
                        *size
                    )
                    .offset(*offset)
                    .build()
                }
                IoRequest::Write { fd, offset, size, buf_index } => {
                    // 类似处理写请求
                }
            };
            
            unsafe {
                ring.submission().push(&sqe)?;
            }
        }
        
        // 一次性提交所有请求
        ring.submit()?;
        
        // 异步等待完成
        tokio::spawn(async move {
            let results = self.wait_batch_completion(batch.requests.len()).await;
            (batch.completion_handler)(results);
        });
        
        Ok(())
    }
}
```

#### 4.2 自适应预读
```rust
pub struct AdaptiveReadahead {
    history: VecDeque<ReadPattern>,
    predictor: ReadPredictor,
    prefetch_queue: Arc<Mutex<VecDeque<PrefetchRequest>>>,
}

impl AdaptiveReadahead {
    pub fn on_read(&mut self, path: &Path, offset: u64, size: u32) {
        // 记录读取模式
        self.history.push_back(ReadPattern {
            path: path.to_owned(),
            offset,
            size,
            timestamp: Instant::now(),
        });
        
        // 预测下一次读取
        if let Some(prediction) = self.predictor.predict(&self.history) {
            // 提交预读请求
            self.submit_prefetch(prediction);
        }
    }
    
    fn submit_prefetch(&self, prediction: ReadPrediction) {
        let mut queue = self.prefetch_queue.lock();
        queue.push_back(PrefetchRequest {
            path: prediction.path,
            offset: prediction.offset,
            size: prediction.size,
            priority: prediction.confidence,
        });
    }
}
```

## 性能监控

### 关键指标
```rust
pub struct IoUringMetrics {
    pub submissions: AtomicU64,
    pub completions: AtomicU64,
    pub sq_full_events: AtomicU64,
    pub cq_overflow_events: AtomicU64,
    pub avg_latency_us: AtomicU64,
    pub p99_latency_us: AtomicU64,
}

impl IoUringMetrics {
    pub fn record_operation(&self, start: Instant, bytes: usize) {
        let duration = start.elapsed();
        let latency_us = duration.as_micros() as u64;
        
        self.completions.fetch_add(1, Ordering::Relaxed);
        
        // 更新延迟统计
        self.update_latency_stats(latency_us);
        
        // 计算吞吐量
        let throughput_mbps = (bytes as f64 / duration.as_secs_f64()) / (1024.0 * 1024.0);
        
        if throughput_mbps > 1000.0 {
            tracing::info!("🚀 io_uring: {:.1} MB/s, latency: {} μs", 
                throughput_mbps, latency_us);
        }
    }
}
```

## 部署策略

### 1. 内核要求
- Linux 5.10+ (基础 io_uring 支持)
- Linux 5.11+ (优化的 splice 支持)
- Linux 5.19+ (完整的固定缓冲区支持)

### 2. 编译时配置
```bash
# 启用 io_uring 特性
cargo build --release --features io_uring

# 检查系统支持
./nfs-cachefs --check-io-uring
```

### 3. 运行时配置
```bash
# 挂载时启用 io_uring
mount -t cachefs -o nfs_backend=/mnt/nfs,cache_dir=/mnt/nvme/cache,\
nvme_use_io_uring=true,nvme_queue_depth=256,nvme_polling_mode=true \
cachefs /mnt/cached
```

### 4. 性能调优
```bash
# 系统参数优化
echo 2048 > /proc/sys/fs/aio-max-nr
echo 2048 > /proc/sys/fs/aio-nr

# CPU 亲和性设置
taskset -c 0-3 ./nfs-cachefs ...

# 内存大页配置
echo 1024 > /proc/sys/vm/nr_hugepages
```

## 测试方案

### 1. 功能测试
- io_uring 可用性检测
- 降级到传统I/O的兼容性
- 错误处理和恢复

### 2. 性能测试
```bash
# 基准测试
fio --name=read --ioengine=io_uring --direct=1 \
    --rw=read --bs=4M --size=10G --numjobs=1

# 对比测试
./benchmark.sh --before-uring --after-uring
```

### 3. 压力测试
- 高并发读取场景
- 混合读写工作负载
- 长时间稳定性测试

## 风险与缓解

### 1. 技术风险
- **内核兼容性**: 提供运行时检测和自动降级
- **稳定性问题**: 充分测试，保留传统I/O路径
- **复杂度增加**: 模块化设计，清晰的抽象层

### 2. 性能风险
- **小文件性能**: 保留原有优化路径
- **内存使用**: 智能缓冲区管理
- **CPU开销**: 自适应轮询策略

## 实施时间表

### 第1周
- [x] 项目分析和方案设计
- [ ] 添加 io_uring 依赖
- [ ] 实现基础 io_uring 模块

### 第2周
- [ ] 改造缓存读取路径
- [ ] 实现固定缓冲区池
- [ ] 基础性能测试

### 第3周
- [ ] 优化缓存写入流程
- [ ] 实现零拷贝传输
- [ ] 集成测试

### 第4-5周
- [ ] 高级特性实现
- [ ] 性能调优
- [ ] 文档和发布准备

## 总结

通过集成 io_uring，NFS-CacheFS 将实现质的飞跃：

1. **极致性能**: 接近硬件理论极限
2. **低延迟**: 微秒级响应时间
3. **高效率**: CPU使用率大幅降低
4. **可扩展**: 支持更高并发和吞吐

这是一个渐进式的改造方案，确保在提升性能的同时保持系统稳定性和兼容性。