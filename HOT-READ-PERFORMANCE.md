# 缓存命中时的读取性能：能否接近 NVMe 直读

> 测试日期：2026-10-31 至 2026-11-01，nfs-cachefs 1.1.0，Ubuntu 24.04.5。
> 状态：6.8（Ubuntu 24.04 默认内核）和 7.0（24.04 HWE 内核）都已测完，7.0 的结果见第 6 节。

## 1. 结论

**能做到。需要两步：调大 NFS 预读（任何内核都要），以及用 ≥ 6.12 的内核（单个读者也要跑满时）。**

| 缓存命中时（GB/s） | 6.8，默认预读 | 6.8，预读调大 | 7.0，预读调大 | 盘上限（O_DIRECT） |
|---|---|---|---|---|
| 1 个读者 `read()` | 1.79 | 2.07 | **3.08–3.24** | 3.0–3.2 |
| 1 个读者 mmap | 0.90 | 1.61 | 2.44 | — |
| 2–4 个读者 | `read()` 3.03，mmap 1.46 | **3.06–3.28** | **3.19–3.28** | 3.19–3.27 |

- **多个读者并行：调大预读就能跑满盘，6.8 上就行。** 把 fsc 挂载的 `read_ahead_kb` 从默认的
  128 调到 4096 以后，2 个以上读者并行（不同文件，或同一文件的不同区段）达到 3.2–3.3 GB/s，
  等于这块 NVMe 用 O_DIRECT 测到的上限。并行加载多个分片的推理框架属于这种情况。
- **只有一个读者：6.8 上最多约 2.0 GB/s（盘上限的 65–70%），7.0 上能到盘上限。** 6.8 内核里
  NFS 的页缓存只用 4 KiB 页，读进程被逐页的分配、清零、记账、插入页缓存占满一个核。上游从
  6.12 起给 NFS 启用了大 folio：7.0 上同样读 10 GB，folio 数从 244 万降到约 5,600，单读者
  `read()` 在预读 16384 KB 时 3.08 GB/s，65536 KB 时 3.24 GB/s。
- **默认预读伤害最大的是 mmap 加载**（safetensors 一类）：每次只读 128 KB，两个内核上缓存命中
  都只有约 1 GB/s，4 个读者也只有 1.5–1.6 GB/s。调大预读后，6.8 单读者 1.6，7.0 单读者 2.44，
  多读者都能跑满。
- 在第二台更新的机器上（Sapphire Rapids + PCIe Gen4 NVMe）抽测，7.0 上单读者热读 4.1–4.2 GB/s，约为这块盘
  单路 O_DIRECT 的 80–90%（第 6.6 节）。
- netfs/cachefiles 本身的 CPU 开销不到 2%，守护进程不在数据路径上，改守护进程解决不了这个问题。
- 7.0 上数据校验通过：6.8 写入、7.0 读出的缓存，以及 7.0 自己写入的缓存，都和服务器上的数据
  一致。

## 2. 测试环境与方法

**测试机**

| 项目 | 配置 |
|---|---|
| CPU | 2 × Intel Xeon Platinum 8160（Skylake-SP，2.1 GHz，睿频 3.7 GHz），96 线程；Spectre/Retbleed（IBRS）、Meltdown（PTI）等缓解为默认开启 |
| 内存 | 503 GiB |
| 缓存盘 | 单块 WD Ultrastar DC SN640 7.68 TB，PCIe Gen3 x4，`max_sectors_kb=128`；XFS，缓存目录自绑定挂载 |
| 系统 | Ubuntu 24.04.5，内核 6.8.0-147-generic（第 6 节换成 7.0.0-42-generic），nfs-common 2.6.4 |
| NFS | NFSv3，`rsize=1M`，`nconnect=4`，`fsc,nosharecache`；nfs-cachefs 1.1.0 默认阈值 |

**方法**

- 测试文件：NFS 上 5 个 safetensors 模型文件，9.8–17 GB。先冷读一遍写入缓存。缓存里的对应文件
  完整，没有空洞；文件系统是新的，文件很连续（10 GB 只有 85 个 extent）。
- 每次测试前后只丢掉测试文件自己的页缓存（`posix_fadvise(POSIX_FADV_DONTNEED)`），不用
  `drop_caches`；用 mincore 确认测试开始时页缓存里没有这个文件。
- 读法：
  - `read()`：顺序 1 MiB 读。
  - mmap：映射整个文件，按顺序每次拷出 64 MiB，模拟先 mmap 再拷贝张量的加载方式。
- NVMe 上限：直接用 O_DIRECT 读该文件在缓存目录里对应的那个文件（同一份数据、同样的布局），
  单路改变块大小，多路用 16 MiB 块。
- 只改 fsc 挂载的 bdi（`/sys/class/bdi/<主:次设备号>/read_ahead_kb`），测完恢复。
- `/proc/fs/netfs/stats` 的 `RA`（预读次数）、`RD`（从缓存读）、`DL`（回源下载）确认每次热读
  完全来自缓存（`DL` 不变），并据此算出平均每次预读的大小。
- 用 perf 采 CPU 分布；用 tracepoint `filemap:mm_filemap_add_to_page_cache` 数插入页缓存的
  folio 个数。

## 3. 实测结果（6.8 内核，单位 GB/s）

### 3.1 热读（数据在本地缓存里）

单个读者，不同的 `read_ahead_kb`：

| 读法 | 128（默认） | 1024 | 4096 | 16384 | 65536 |
|---|---|---|---|---|---|
| `read()`，1 MiB | 1.79 | 1.78 | 2.07 | 2.07 | 2.05 |
| mmap 顺序拷贝 | **0.90** | 1.55 | 1.61 | 1.62 | 1.59 |
| 平均每次预读 | 1 MiB / 128 KB | 1 MiB | 4 MiB | 16 MiB | 64 MiB |

- 默认设置下读进程占 90% CPU；`read_ahead_kb` 达到 4096 以后占 100%，已经被 CPU 卡住。

多个读者并行（`read_ahead_kb=4096`），以及 NVMe 上限：

| 读者数 | fsc `read()` | fsc mmap | NVMe O_DIRECT（16 MiB 块） |
|---|---|---|---|
| 1 | 2.01–2.07 | 1.61 | 2.81 |
| 2（2 个文件） | **3.25** | 3.06 | 3.19 |
| 2（同一文件分两段） | **3.25** | — | — |
| 4（4 个文件） | **3.22** | **3.16** | 3.22 |
| 4（同一文件分四段） | **3.28** | — | — |

- 默认 `read_ahead_kb=128` 时，4 个读者 `read()` 是 3.03，mmap 只有 1.46。
- 2 个读者达到 3.25 GB/s 时，整机 CPU 占用约 1.8 个核。
- NVMe 单路 O_DIRECT 和块大小的关系：1 MiB 1.4，4 MiB 2.3，16 MiB 2.7–2.8，64 MiB 2.9。

### 3.2 冷读（回源 NFS 服务器，单个读者，非高峰时段）

| 读法 | 128（默认） | 4096 | 16384 |
|---|---|---|---|
| mmap 顺序拷贝 | 0.41 | 1.09 | — |
| `read()` | 0.50（`dd bs=4M`） | 1.33 | 1.80 |

默认设置下 mmap 每次回源只读 128 KB（9.8 GB 发了 75,067 次预读）。冷读速度还受服务器负载和网络
影响，上表只用于比较不同设置。

### 3.3 对照：本地 XFS 文件（缓冲读，1 MiB）

读的是缓存目录里同一份数据，改的是 NVMe 自己的 `read_ahead_kb`：

| 读法 | 吞吐 | 插入页缓存的 folio 数 | 读进程 CPU |
|---|---|---|---|
| XFS，`read_ahead_kb=128`（默认） | 0.83–0.97 | 75,076（平均 128 KiB） | 5.2 s，0.44 核 |
| XFS，`read_ahead_kb=4096` | **3.0** | 4,900（平均约 2 MiB） | 2.85 s，0.86 核 |
| NFS fsc 热读，`read_ahead_kb=4096` | 2.07 | 2,438,549（每个 4 KiB） | 4.8 s，1.0 核（占满） |

- 如果"NVMe 直读"指的是按默认设置读本地文件，那只有 0.83–0.97 GB/s，现在的 fsc 热读已经比它快。
- 同一内核下，XFS 的单个读者能跑满盘，NFS 不能。差别就在 folio 大小（第 4.2 节）。

## 4. 原因分析

### 4.1 预读窗口决定每次读盘的大小

- 内核按预读窗口往页缓存读数据。对 fsc 挂载，每个窗口变成一个 netfs 请求；缓存命中时，
  cachefiles 对缓存文件发一个同样大小的 O_DIRECT 读。窗口越大，NVMe 上同时排队的请求越多
  （块层按 `max_sectors_kb` 切成多个 128 KB 命令并行下发）。
- `read()` 的窗口取 `read_ahead_kb` 和 `min(请求大小, io_pages)` 中较大的一个。NFS 的
  `io_pages` 等于 `rsize`（1 MiB），所以默认设置下 1 MiB 的 `read()` 也有 1 MiB 窗口：单个
  1 MiB 的 O_DIRECT 只有 1.4 GB/s，异步预读让两个窗口重叠，得到 1.79。
- mmap 缺页时的预读（read-around）只看 `read_ahead_kb`，默认就是 128 KB 一次，所以 mmap 加载
  热读只有 0.9 GB/s，冷读也是一次一个 128 KB 的 NFS READ。
- 冷读同理：窗口按 `rsize` 切成多个 NFS READ 并行发出，窗口越大，单个读者并发的 RPC 越多。
- NVMe 自己的 `read_ahead_kb` 默认也是 128，所以本地 XFS 文件按默认设置缓冲读也只有约 0.9 GB/s。

### 4.2 单读者被 CPU 卡住：6.8 的 NFS 页缓存只用 4 KiB 页

单读者 `read()`、`read_ahead_kb=16384` 时的 perf 采样（读进程占全部样本的 94%）：

| 开销 | 占比 |
|---|---|
| 拷贝到用户态（`rep_movs_alternative`） | 17.6% |
| 插入页缓存（`__filemap_add_folio`、xarray） | 约 16% |
| memcg 记账（`try_charge_memcg`、`__mem_cgroup_charge` 等） | 约 13% |
| 页分配时清零（`clear_page_erms`，Ubuntu 默认 `init_on_alloc=1`） | 6.6% |
| RCU、页分配、LRU、`folio_mark_accessed` 等逐页操作 | 约 20% |
| netfs + cachefiles（`netfs_readahead`、`netfs_rreq_unlock_folios` 等） | < 2% |

- 整机 cycles 约等于一个核跑在 3.7 GHz，IPC 0.71。
- 6.8 的 NFS 没有启用大 folio，每个 folio 4 KiB，读 10 GB 要插入约 244 万个；XFS 用大 folio，
  同样的数据只有 4,900 个，所以同一内核下 XFS 的单读者能跑满盘。
- 上游从 **v6.12** 起在 `fs/nfs/inode.c` 里给普通文件调用 `mapping_set_large_folios()`
  （v6.11 没有，v6.12 到 v7.0 都有）。
- 这台测试机的 CPU 老、缓解措施开销大，逐页的内核开销被放大。新一些的 CPU 上，单读者的数字会
  更高，但结构相同。

## 5. 建议

### 5.1 调大 NFS 预读（马上能做，不用重启）

- 在线修改，对之后打开的文件生效（已经打开的文件仍按原值）：

  ```sh
  echo 16384 > /sys/class/bdi/$(mountpoint -d /mnt/llm-data)/read_ahead_kb
  ```

- 持久设置：nfs-common 2.6.2 起自带 `/usr/libexec/nfsrahead` 和
  `/usr/lib/udev/rules.d/99-nfs.rules`，每次挂载 NFS 时按 `/etc/nfs.conf`（及
  `/etc/nfs.conf.d/*.conf`）里的 `[nfsrahead]` 设置预读。Ubuntu 24.04 的 nfs-common 2.6.4 已经带上，
  只缺配置：

  ```ini
  # /etc/nfs.conf.d/readahead.conf
  [nfsrahead]
  nfs=16384
  nfs4=16384
  ```

- 写好配置后，可以对已经挂载的 NFS 重放一次 udev 的 add 事件，让 nfsrahead 立即按新配置设置。
  这和挂载时走的是同一条路径，也顺便验证了配置有效：

  ```sh
  udevadm trigger --action=add --settle "$(readlink -f /sys/class/bdi/$(mountpoint -d /mnt/llm-data))"
  cat /sys/class/bdi/$(mountpoint -d /mnt/llm-data)/read_ahead_kb
  journalctl --since -1min | grep nfsrahead   # setting /mnt/llm-data readahead to 16384
  ```

  测试机上验证过两种情况：对已有挂载重放 add 事件，以及新挂载（automount 首次访问），都设成了配置值。
- 取值：6.8 上只看缓存命中，4096 就够了（16384、65536 与 4096 相同）；16384 能让单读者冷读再快约
  35%（1.33 → 1.80）。7.0 上单读者 `read()` 随预读继续涨（4096：2.83，16384：3.08，65536：3.24），
  mmap 在 4096 以上不再变化。测试机最终设为 16384。
- 注意事项：
  - `nfs=` 对本机所有 NFSv3 挂载生效，不能只针对 fsc 挂载。
  - 单个读者冷读更快，NFS 服务器在高峰期承受的瞬时压力会更集中（总流量不变）。
  - 随机访问的 mmap 每次缺页可能多读一个窗口，直到内核的 `mmap_miss` 判断（约 100 次未命中）
    关掉 read-around；随机 `read()` 不受影响，预读算法能识别出来。
  - 守护进程不能自己设置：systemd unit 开了 `ProtectKernelTunables=true`，`/sys` 是只读的。
    比较合适的做法是在 `install.sh` 里加一个类似 `NFS_READAHEAD_KB` 的选项，写上面的配置并
    设置正在使用的挂载。

### 5.2 让单个读者也接近盘上限：内核 ≥ 6.12

- Ubuntu 24.04 的 HWE 内核（`linux-generic-hwe-24.04`）目前是 7.0.0-42，已包含 NFS 大 folio。
  实测单读者 `read()` 到了盘上限，mmap 从 1.61 提到 2.44，数据校验通过（第 6 节）。
- 换内核要重启，要为树外驱动重新编译 DKMS（测试机上的 GPU 驱动通过 DKMS 自动编译成功）。
- 6.12 前后 netfs 的读路径改动很大。7.0 上的绑定、读缓存、写缓存都已验证，但 **cull 还没有在
  7.0 上实测过**（要等缓存盘空间紧张时才会触发），推广前要补上。

### 5.3 应用侧并行读

在现有内核上，2 个以上读者并行就能跑满盘：同时加载多个分片，或把一个大文件分段读。
一次 mmap 一个文件的单线程加载要靠第 5.1 节（6.8 上 0.9 → 1.6 GB/s）和第 5.2 节（7.0 上 2.44 GB/s）。

### 5.4 不建议的做法

- 用户态或 FUSE 数据路径：多一次拷贝和上下文切换，只会更慢（项目已经放弃过 FUSE 方案）。
- `mitigations=off`、`init_on_alloc=0`：多租户主机上是安全上的倒退，而且 `init_on_alloc`
  只占约 7%。

## 6. 7.0 内核对比测试

同一台测试机、同样的文件和脚本，换成 Ubuntu 24.04 HWE 内核 7.0.0-42-generic（2026-11-01）。
nfs-cachefs 1.1.0 不需要改动，绑定正常。缓存卷标识和 6.8 下一样，换内核后原来的缓存直接命中。
以下单位是 GB/s，括号里是 6.8 的数。

### 6.1 单个读者

| `read_ahead_kb` | `read()` | mmap 顺序拷贝 | `read()` 时读进程 CPU | 插入的 folio 数（10 GB） |
|---|---|---|---|---|
| 128（默认） | 2.06（1.79） | 0.97（0.90） | 0.61 核（0.90） | 9,864（2,438,549） |
| 1024 | 2.07（1.78） | 1.87（1.55） | 0.61 核（0.90） | — |
| 4096 | 2.83（2.07） | 2.44（1.61） | 0.82 核（1.0） | 5,609 |
| 16384 | **3.08**（2.07） | 2.43（1.62） | 0.94 核（1.0） | 5,719 |
| 65536 | **3.24**（2.05） | 2.40（1.59） | 0.99 核（1.0） | — |

- 7.0 上 NVMe 单路 O_DIRECT：1 MiB 1.4，4 MiB 2.5，16 MiB 3.0，64 MiB 3.2；多路 3.26–3.27。
  `read()` 单读者已到盘上限。
- mmap 单读者在 2.4 左右封顶，读进程占满一个核（缺页处理加拷贝），预读再大也不变。

### 6.2 多个读者（`read_ahead_kb=4096`）

| 读法 | 7.0 | 6.8 |
|---|---|---|
| `read()`，2 个文件 | 3.19 | 3.25 |
| `read()`，4 个文件 | 3.26 | 3.22 |
| `read()`，同一文件分两段 | 3.28 | 3.25 |
| mmap，2 个文件 | 3.21 | 3.06 |
| mmap，4 个文件 | 3.23 | 3.16 |
| mmap，4 个文件，默认预读 128 | 1.58 | 1.46 |
| `read()`，4 个文件，默认预读 128 | 3.18 | 3.03 |

- 都能跑满盘。4 个读者 `read()` 时整机 CPU 占用 1.6 核，6.8 是 2.5 核。

### 6.3 冷读（`read_ahead_kb=4096`，回源服务器）

`read()` 1.31（6.8：1.33），mmap 1.20（6.8：1.09）。冷读主要受服务器和网络限制，换内核差别不大。
7.0 写出的缓存文件完整、无空洞，17.7 GB 只有 18 个 extent。

### 6.4 数据正确性

比较经缓存读出的数据和用 O_DIRECT 绕过缓存从服务器读出的数据（BLAKE2b）：

- 6.8 写入的缓存文件，6.8 和 7.0 上读出的结果都和服务器一致。
- 7.0 上冷读写入缓存、再丢掉页缓存从缓存读出，结果和服务器一致。
- 这两次读都完全来自缓存：netfs 只有 `RD`（读缓存）增加，`DL`（回源）不变。

### 6.5 换到 7.0 时要注意

- `/proc/fs/netfs/stats` 的格式变了：6.8 每行以 `Netfs` 开头，6.12 起改成 `Reads`、`DownOps`、
  `CaRdOps`、`CaWrOps` 等。解析它的工具要两种都支持；`RA`、`RD`、`DL` 这些字段名没变。
- cull 还没有在 7.0 上触发过，推广前要实测。
- 树外 GPU 驱动如果当初没有注册 DKMS（比如 runfile 默认不带 `--dkms`），新内核下就没有驱动模块。
  换内核前要先把驱动重装成 DKMS 方式，并为新旧两个内核都编译好，旧内核才能当作备用。
- Ubuntu 的 `perf` 包装脚本要求装有和当前内核对应的 linux-tools；没装时可以直接运行其他版本的
  perf 二进制（`/usr/lib/linux-tools/<版本>/perf`），计数和采样都正常。

### 6.6 第二台机器抽测（7.0）

另一台机器：2 × Intel Xeon Platinum 8481C（Sapphire Rapids），单块 Solidigm 7.68 TB PCIe Gen4 x4，
内核 7.0.0-42，`read_ahead_kb=16384`。这台只测了单读者 `read()` 和数据正确性：

| 项目 | GB/s |
|---|---|
| fsc 热读，单读者 `read()` | 4.1–4.2 |
| NVMe O_DIRECT，16 MiB / 64 MiB 块 | 4.7 / 5.4 |

- 单读者约为这块盘单路 O_DIRECT 的 80–90%。多读者和 mmap 没有在这台上测。
- 先冷读写入缓存、再从缓存读出、再用 O_DIRECT 从服务器读，三者的 BLAKE2b 一致。

## 7. 复现方法

```sh
F=/mnt/llm-data/<大文件>
BDI=$(mountpoint -d /mnt/llm-data)
cat /sys/class/bdi/$BDI/read_ahead_kb

dd if="$F" iflag=nocache count=0 status=none          # 只丢这个文件的页缓存
grep -E 'RA=|RD=|DL=' /proc/fs/netfs/stats              # 记下 RA / RD / DL
dd if="$F" of=/dev/null bs=1M                           # 热读
grep -E 'RA=|RD=|DL=' /proc/fs/netfs/stats              # DL 不变 = 全部来自缓存

# 插入页缓存的 folio 个数（读一遍 10 GB：约 244 万个说明是 4 KiB 页）
dd if="$F" iflag=nocache count=0 status=none
perf stat -e filemap:mm_filemap_add_to_page_cache -- dd if="$F" of=/dev/null bs=1M

# 盘上限：直接 O_DIRECT 读缓存目录里对应的那个文件
dd if=<cache_dir>/cache/<卷目录>/<@xx>/<对象> of=/dev/null bs=16M iflag=direct
```

- 新对象在使用期间是匿名临时文件，cookie 闲置约 10 秒后才链接进缓存目录，读完要等一会儿才能
  在缓存目录里找到它。
- mmap 加载可以用下面几行模拟：

  ```python
  import mmap, os
  fd = os.open(F, os.O_RDONLY); size = os.fstat(fd).st_size
  mv = memoryview(mmap.mmap(fd, size, prot=mmap.PROT_READ)); out = bytearray(64 << 20)
  for off in range(0, size, len(out)):
      n = min(len(out), size - off); out[:n] = mv[off:off + n]
  ```

## 8. 待办

- 在 ≥ 6.12 内核上实测 cull（缓存盘空间紧张时的淘汰路径）。7.0 上的绑定、读缓存、写缓存和数据
  正确性都已验证，cull 还没有触发过，在这之前不建议在生产上换 7.0。
- `install.sh` 增加预读选项（如 `NFS_READAHEAD_KB`，写 `/etc/nfs.conf.d/` 并对已有挂载重放 udev
  事件）：暂缓，先在测试机上观察 16384 的实际效果。
