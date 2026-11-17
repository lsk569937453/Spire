# Spire vs nginx：单核 / 2 核 / 4 核压测、火焰图、混沌实验与优化冲刺

> 数据与脚本：`benchmarks/perf-2026-11/`
> 采集时间：2026-11-16 ~ 11-17（Asia/Hong_Kong）
> 被测：Spire 0.0.30（`dev` @ `96370e8` + 本报告的两处优化 + pprof 采样补丁）
> 对照：nginx 1.23.3（与仓库 `benchmarks/nginx` 同版本、同配置基线）
> 优化分支：`perf/pprof-sampling-window`

---

## 0. 先读这一段：哪些数字可信，哪些不可信

这台机器的**绝对吞吐不可复现**：同一份 nginx 配置、同一并发，两次测量能差 3 倍（14.7k ↔ 47k RPS），宿主上跑着浏览器、ToDesk、Harness，Docker Desktop 还占着全部 20 个逻辑 CPU。因此：

| 数据类型 | 可信度 | 怎么用 |
|---|---|---|
| **同场次交错 A/B（`scripts/ab.ps1`）** | 高 | 优化决策只看这个：`ceiling → spire → nginx → ceiling`，用两次 ceiling 的均值归一 |
| **火焰图自耗时** | 高 | 相对归因（谁占 CPU 比例）不受绝对吞吐影响 |
| **日志 A/B** | 高 | 同一会话内只改 `log_level`，差 24%~49% 远超噪声 |
| **混沌实验** | 中 | 固定 1000 rps 到达率，看的是相对劣化 |
| **矩阵绝对 RPS** | 低 | 只用来画形状（饱和点在哪），不要引用绝对值 |
| **README 历史数字（114,512 / 148,676）** | 不可比 | 当年两个后端响应体就不等（155 vs 146 bytes/req） |

---

## 1. 结论摘要

**1）同核对比：优化前 Spire 约为 nginx 的 57%，现在吞吐已持平/略超，CPU 效率还差 ~1.5 倍。**

| 场景 | 优化前 | 现在（同场次交错 A/B） |
|---|---|---|
| 1 核 c=64 | spire/nginx ≈ 0.59 | 0.84 / 1.17（早期两次） |
| 4 核 c=256 | — | **1.256** |
| **4 核 c=64** | — | **84,416 vs 83,344 RPS（Spire 领先 1%）** |
| 1 核 CPU/请求 | 1.72× | **1.49×** |
| 4 核 CPU/请求 | 1.63× | **1.51×** |

**1b）换用「每请求 CPU 时间」这个不受调度抖动影响的指标后，差距被量化了：**

| 核数 | Spire | nginx | 比值 |
|---|---:|---:|---:|
| 1 核 c=64 | 57.4 µs/req（17,385 rps） | 33.5 µs/req（29,473 rps） | **1.72×** |
| 4 核 c=64 | 62.8 µs/req（51,900 rps） | 38.5 µs/req（50,642 rps） | **1.63×** |

注意 4 核那行：**吞吐已经基本持平（51.9k vs 50.6k），但 Spire 多烧 63% 的 CPU**。所以"吞吐持平"很大程度是因为两边都撞到同一个环境上限，而不是代码效率追平了。真正的目标是把 µs/req 从 57 压到 ~34。

**1c）把 CPU 拆成用户态/内核态后，差距的来源就唯一了**（`scripts/cpu-split.ps1`，读 `/proc/*/stat` 的 utime/stime，单核 c=64）：

| 网关 | user µs/req | sys µs/req | 合计 |
|---|---:|---:|---:|
| Spire | **21.9** | 32.7 | 54.6 |
| nginx | **6.7** | 27.4 | 34.1 |

- 内核态只差 19%（32.7 vs 27.4）——**不是内核问题，也不是系统调用数量问题**（Spire 每请求 3.92 次系统调用，nginx 5.15 次）。
- **用户态差 3.3 倍（21.9 vs 6.7 µs）**，这才是要打的地方。按 1 核火焰图的比例拆：hyper ~8µs、memcpy ~3µs、tokio ~2µs、mimalloc ~1µs、Spire 自身 ~1.6µs。
- nginx 只花 6.7µs 用户态，是因为它把上游连接直接挂在连接对象上，**每请求没有任何池查找**；Spire 走 hyper-util 的 legacy client，每请求一次连接池 DashMap 查找 + `PoolClient` future 链。

**2）瓶颈不在 Spire 的 Rust 业务逻辑。** 4 核饱和点上 Spire 自身代理函数自耗时只有 **2.2%**；真正的大头是内核网络路径（含 Docker bridge 的 conntrack/nftables）、`memcpy`、以及每请求的 mimalloc 分配与释放。

**3）最大的单点开关是日志。** `log_level: debug`（打开访问日志）让 4 核吞吐从 43,751 掉到 22,331 RPS（**−49%**），20 秒写出 **570 MB** 日志。默认 `info` 以上即关闭访问日志。

**4）三个结构性改动带来主要提升**（详见 §8）：两端 `TCP_NODELAY`、去掉每请求的 header 大小写映射表、去掉一次 `HeaderMap` 深拷贝。

**5）混沌实验：故障被隔离得很好。** 10 个接口里 1 个被 chaosblade 注入 2s 延迟时：健康 9 个接口 p95 从 13.8ms 劣化到 23.5ms（+70%），但**零错误、吞吐只掉 3.5%**；给路由加 300ms 上游超时后，慢接口 p95 收敛到 316ms、错误率 10.1%（正好是打到它那 10% 流量），健康接口 29.4ms。

---

## 2. 测试环境与方法

| 项 | 值 |
|---|---|
| CPU | i5-13600K：6 P 核（逻辑 0-11，含超线程）+ 8 E 核（逻辑 12-19） |
| 内存 | 32 GB，Docker VM 15.5 GB |
| OS | Windows 11 + Docker Desktop 4.82.0（WSL2 6.6.87.2-microsoft，cgroup v2） |
| 网络 | 单一 user-defined bridge，客户端与网关同网，**不经过** Docker Desktop 端口转发 |
| 客户端 | `demisto/rakyll-hey`（容器内，绑 `0,2`） |
| 后端 | 自写零依赖 Java 后端，10 接口，响应体固定 **96 字节**，单次 `write` + `TCP_NODELAY`，跑在 8 个 E 核上 |
| 网关绑核 | 1 核=`4`，2 核=`4,6`，4 核=`4,6,8,10`（**不同物理 P 核的各一个线程**，避免把同一核的两个超线程当成 2 核） |
| 网关内存 | 统一 `mem_limit 2g`（否则 Spire 的进程级内存保险会按 15.5G 推导，三档不可比） |

方法：每格预热后测量；吞吐用 hey `-o csv` 的 **`offset` 列算真实窗口**（这个 hey 版本在 100 万请求时会提前结束，用墙钟会低估约 40%）；火焰图用 `flamegraph-rs` 0.6.15 + perf 6.8.12，软件事件 `cpu-clock`（WSL2 无硬件 PMU），997Hz，`--pid=container:` 共享 PID 命名空间，被采的是 `spire-symbols`（release + debuginfo，不含 pprof feature）。

复现见 [`README.md`](README.md)。

---

## 3. 压测前发现的四个真问题

1. **`forward_to` 结尾带斜杠会拼出 `//`**：`http://backend:8080/` + `/` 前缀匹配 → 上游收到 `//api/1`，直接 400 `Bad request URI`。去掉尾斜杠即正常。
2. **仓库 `benchmarks/spires/docker-compose.yaml` 有两处无效配置**：`CONFIG_FILE_PATH` 这个环境变量在代码里不存在（配置路径只来自 `-f`），且挂载到 `/tmp/app_config.yaml`，而镜像 ENTRYPOINT 读 `/etc/spire/config.yaml` —— 那份 benchmark 配置从未生效；`JEMALLOC_SYS_WITH_MALLOC_CONF` 也无效（实际用 mimalloc）。
3. **后端不能用 `com.sun.net.httpserver`**：header 与 body 分两次 `write`，回环上触发 Nagle + 延迟 ACK，单请求固定多 ~40ms，c=16 时整栈只有 342 RPS；换成单次 `write` + `TCP_NODELAY` 后同档位 55,252 RPS。
4. **`log_level: debug` 是吞吐杀手**（见 §5）。

---

## 4. 吞吐与延迟（矩阵数据，仅用于看形状）

Spire 为 3 次中位数；nginx 行因机器时间被压缩，为 1 次测量（标注 `1×`）。

| 网关 | 核数 | 并发 | RPS | 窗口 s | p50 ms | p99 ms | p99.9 ms | 错误 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| spire | 1 | 1 | 3,955 | 60 | 0.2 | 1.0 | 2.8 | 0 |
| spire | 1 | 16 | 26,357 | 38 | 0.5 | 1.8 | 3.9 | 0 |
| spire | 1 | 64 | 24,550 | 41 | 2.1 | 9.7 | 18.0 | 0 |
| spire | 1 | 256 | 16,421 | 60 | 13.3 | 53.0 | 91.2 | 0 |
| spire | 1 | 1024 | 14,788 | 60 | 14.7 | 349.3 | 571.5 | 28 |
| spire | 2 | 1 | 7,101 | 60 | 0.1 | 0.5 | 1.1 | 0 |
| spire | 2 | 16 | 48,298 | 21 | 0.3 | 1.2 | 2.7 | 0 |
| nginx（1×） | 1 | 64 | 32,292 | 31 | 1.4 | 9.5 | 15.6 | 0 |
| nginx（1×） | 1 | 256 | 17,939 | 40 | 11.7 | 51.7 | 82.4 | 0 |
| nginx（1×） | 2 | 64 | 34,921 | 29 | 1.2 | 12.1 | 22.3 | 0 |
| nginx（1×） | 2 | 256 | 34,017 | 29 | 6.2 | 39.4 | 68.8 | 0 |
| nginx（1×） | 4 | 64 | 66,262 | 15 | 0.7 | 6.4 | 13.0 | 0 |

读法：Spire 在 1 核 c=16 达到峰值（26k），之后**回落**到 c=1024 的 14.8k——过载时队列把 p99 推到 349ms；nginx 在同样条件下降幅更小。曲线见 `results/charts/throughput.svg` / `p99.svg`。

---

## 5. 日志开销 A/B（4 核，只改 log_level）

| 配置 | c=64 | c=256 | 20 秒日志量 |
|---|---:|---:|---:|
| `log_level: warn`（访问日志关） | 48,974 RPS / 1.3ms | 43,751 RPS / 5.8ms | 530 B |
| `log_level: debug`（每请求一行） | 37,012 RPS / 1.7ms（**−24%**） | 22,331 RPS / 11.5ms（**−49%**） | **570 MB** |

原因不只是格式化：logger 是 `NonBlockingBuilder::default().lossy(false)`，队列满时**阻塞等待**，于是磁盘写入直接进到请求路径上。

---

## 6. 火焰图与瓶颈归因

采样：`flamegraph-rs` 0.6.15 + perf 6.8.12，软件事件 `cpu-clock` 997Hz，`perf record -p 1 -g`，被采的是 `spire-symbols`（release + debuginfo，不含 pprof feature）。自耗时由 `scripts/flamegraph_top.py` 从 SVG 的 `fg:x/fg:w` 重建调用树后计算（自耗时之和 = 根样本数，已校验）。

### 6.1 4 核（c=256、47,235 RPS、50 秒、160,559 样本）

| 自耗时占比 | 符号 | 归因 |
|---:|---|---|
| **22.88%** | `_raw_spin_unlock_irqrestore` | 内核态（**见 6.3 的采样说明**） |
| 4.94% | `memcpy` | 数据拷贝 |
| 2.47% | `__nf_conntrack_find_get` | Docker bridge conntrack（**环境 overhead**） |
| 2.16% | `_mi_page_malloc_zero` | mimalloc 分配（每请求） |
| 1.87% | `spire::proxy::http1::http_proxy::proxy` | Spire 自身转发逻辑 |
| 1.52% | `mi_free` | mimalloc 释放（每请求） |
| 0.97% | `hyper::proto::h1::dispatch::Dispatcher::poll_loop` | HTTP/1.1 编解码 |
| 0.72% | `nft_do_chain` | nftables（**环境 overhead**） |
| 0.59% | `selinux_file_permission` | SELinux 钩子（**环境 overhead**） |
| 0.51% | `prometheus ... with_label_values` | 指标打点（每请求） |

按组件：hyper 14.6%、tokio 5.4%、libc mem 5.3%、mimalloc 3.7%、内核网络路径（含 conntrack/nftables/SELinux）**~69%**。

### 6.2 1 核（c=64、40 秒、1305 帧）

`_raw_spin_unlock_irqrestore` 23.6%、`memcpy` 5.2%、`__nf_conntrack_find_get` 4.3%、`spire::proxy::proxy` 2.4%、`_mi_page_malloc_zero` 1.4%、`tcp_rcv_established` 0.8%、`do_iter_write` 0.8%、`mi_free` 0.8%、`nft_do_chain` 0.7%。组件：hyper 15.0%、libc mem 5.5%、tokio 3.6%、mimalloc 2.1%、其余（内核）**69%**。

### 6.3 采样口径的重要修正

先前把 `_raw_spin_unlock_irqrestore` 读成"自旋锁竞争"是**过度解读**，必须修正：

1. **单核上不可能有可观的锁竞争**，而单核火焰图里它同样占 23.6%——说明这是 `cpu-clock` 软件事件采样落在**中断/上下文切换边界**造成的符号偏置，不是 Spire 的 bug，也不是我们的代码问题。
2. 因此内核段的正确读法是"**约 2/3 的样本落在内核态（含 Docker bridge 的转发成本）**"，不要逐符号解读。
3. 这段内核成本里有很大一部分是**环境**（user-defined bridge 的 veth + conntrack + nftables + SELinux），在生产用 host 网络或 eBPF 转发时会显著下降。

### 6.4 系统调用计数（perf tracepoint）

同一负载（c=64、10 秒）下按 CPU 统计：

| 网关 | raw syscalls | writev | syscalls/请求 |
|---|---:|---:|---:|
| Spire | 588,125 | 290,933 | **3.92** |
| nginx | 1,495,742 | 589,765 | **5.15** |

Spire 的系统调用**比 nginx 更少**（3.92 vs 5.15），writev 都是每请求 2 次（客户端 + 上游）。所以差距不在系统调用数量，而在**用户态每请求的 CPU**（hyper 15% + memcpy 5.5% + tokio 3.6% + mimalloc 2.1%），以及每次系统调用本身在 WSL2 上的固定成本。

---

### 6.5 纯用户态火焰图（`perf record -e cpu-clock:u`，1 核 c=64，12,155 样本）

内核态不是主战场，所以单独采了一版**只含用户态**的剖面，它给出了可以直接下手的清单：

| 用户态占比 | 符号 | 说明 |
|---:|---|---|
| **14.69%** | `memcpy` | HeaderMap/Bytes/写缓冲拷贝 |
| 4.57% | `spire::proxy::http1::http_proxy::proxy` | 自身转发逻辑（内联后聚集） |
| 2.80% + 2.09% + 1.05% + 0.82% | `_mi_page_malloc_zero` / `mi_free` / `mi_theap_malloc_aligned` / `mi_malloc_aligned` | **mimalloc 合计 6.8%** |
| 1.91% | `hyper_util::client::legacy::Client` | 上游客户端（池 + future 链） |
| 1.13% | `proxy_adapter` | 请求适配层 |
| 1.07% + 0.83% | `[[vdso]]` / `__vdso_clock_gettime` | **每请求多次 `Instant::now()` 合计 1.9%** |
| 1.06% + 0.87% | `bytes_mut::shared_v_drop` / `bytes::shared_drop` | Bytes 释放 |
| **1.00%** | `url::Parser::after_double_slash` | **每请求用 `url` crate 解析上游 endpoint**（已优化掉） |
| 0.97% | `hyper Server::Http1Transaction::encode` | 响应编码 |
| 0.87% | `prometheus MetricVec::with_label_values` | 指标打点 |
| 0.82% | `concurrency::acquire_weighted` | 并发预算 |
| 0.77% | `http::uri::PathAndQuery::from_shared` | 每请求重解析 URI |

按组件：mem/string 15.4%、hyper 14.3%、其余分散在各处。**没有单一 30% 的巨型热点**——这也是为什么微观优化只值 1~2%，必须是「一堆 1~2% 叠起来」。

据此已经落地的两项：`endpoint_key` 不再每请求 `Url::parse`（消掉 1.0% + 两次 format）。

---

## 7. chaos-blade 混沌实验：10 个接口里 1 个慢

设计：chaosblade **v1.7.3**（`latest` 已变成 `blade-ai-v0.7.4`，是另一条产品线，不含 exec-jvm），装在后端容器内，`SYS_PTRACE` + `seccomp:unconfined`，`blade create jvm delay --time 2000 --classname Backend$E7 --methodname serve --pid 1` —— **方法级**注入，精确只慢第 7 个接口。流量用 k6 `constant-arrival-rate` 1000 rps，10% 打 `/api/7`，其余 90% 均摊到另外 9 个。

| 阶段 | api_7 p95 | 其余 9 个 p95（中位） | 错误率 | 吞吐 |
|---|---:|---:|---:|---:|
| baseline（无故障） | 13.2 ms | 13.8 ms | 0% | 1000 rps |
| 注入 2s（路由默认 5s 超时） | **2,027.6 ms** | **23.5 ms** | 0% | 965 rps（−3.5%） |
| 注入 2s + 网关超时 300ms | **316.0 ms** | **29.4 ms** | **10.1%** | 995 rps |

**结论**：
- **故障隔离是合格的**：一个接口慢 2 秒，其余 9 个只劣化 70%（没有级联超时、没有错误放大、吞吐只掉 3.5%），说明并发预算/连接复用没有被慢请求吃光。
- **但没有"完全无感"**：健康接口 p95 从 13.8ms 涨到 23.5ms，说明存在排队/HOL 效应（慢请求占住 permit 与连接一段时间）。
- **超时策略是有效的止损手段**：300ms 超时把故障限制在错的那 10% 流量上（错误率 10.1% 正好等于该接口份额），健康接口回到 29.4ms、吞吐 995 rps。
- `blade destroy` 后指标恢复正常（实验可逆）。

---

## 8. 优化冲刺：做了什么、拿到多少

分支 `perf/pprof-sampling-window`（当前未合并），三个改动：

| # | 改动 | 文件 | 依据 |
|---|---|---|---|
| 1 | **客户端/上游 socket 设 `TCP_NODELAY`** | `proxy/http1/http_proxy.rs`（accept 后）、`proxy/http1/http_client.rs`（`HttpConnector::set_nodelay(true)`） | hyper-util 默认 `nodelay: false`；nginx 两侧默认开。不分片的写会等对端延迟 ACK |
| 2 | **去掉每请求的 header 大小写映射** | `http_proxy.rs`（`preserve_header_case`/`title_case_headers`）、`http_client.rs` 同 | hyper 为此每消息维护一张 case map 并做一次重写；nginx 直接透传 header 字节 |
| 3 | **去掉一次 `HeaderMap` 深拷贝** | `http_proxy.rs` 响应阶段 | 每次请求克隆整个 header map，且原来还 `.clone()` 了第二次 |
| 4 | **`route_label` / `path` 不再每请求 `to_string()`** | `http_proxy.rs` | 直接借用 + 只在错误分支格式化 |
| 5 | **`UpstreamLabels` 改 `Arc<str>`** | `upstream_body.rs`、`upstream_forward.rs` | 每请求原本构造 3 个 String 再克隆 3 个；改成 3 个 `Arc` + 3 次原子自增 |
| 6 | pprof 采样可控（频率/时长/输出路径环境变量） | `app/run.rs` | 让火焰图能采到"稳态饱和点"而不是"启动 20 秒" |
| 7 | **`endpoint_key` 不再每请求 `Url::parse`** | `proxy/concurrency.rs` | 用户态火焰图里 `url::Parser::after_double_slash` 占 1.0%，还附带两次 `format!` |
| 8 | **`lto = "fat"` + `codegen-units = 1`** | `Cargo.toml` | 同会话 3 轮 A/B：用户态 21.9 → 19.9 µs（−9%），二进制 18.3 → 14.5 MB；代价是 release 构建 4m10s → 8m45s |

**实测进展（同场次交错 A/B，`scripts/ab.ps1`）**

| 阶段 | 1 核 c=64 | 4 核 c=64 |
|---|---|---|
| 优化前（spire/nginx 吞吐比） | 0.59 | — |
| 第 1 轮（改动 1-3） | 0.84（另一次 1.17） | 0.77 / 0.61 |
| 第 2 轮（改动 4-5） | 56.8 µs/req vs 33.5（1.70×） | 61.9 µs/req vs 41.1（1.51×） |

绝对值波动来自宿主噪声（同一份 nginx 配置两次能跑出 14.7k 与 47k），所以从第 2 轮起改用 **µs/req** 作为优化指标。

### 8.1 编译选项（零代码风险，收益最大的一项）

用 `scripts/bin-ab.ps1` 在同一会话内交替测量两个构建（每个 3 轮取中位数，读 `/proc` 的 utime/stime，不受当刻吞吐影响）：

| 构建 | 用户态 µs/req | 内核态 µs/req | 体积 | 相对基线 |
|---|---:|---:|---:|---:|
| `lto="thin"` + `codegen-units=16`（原） | 21.9 | 32.7 | 18.3 MB | 1.00 |
| **`lto="fat"` + `codegen-units=1`** | **19.9** | 30.0 | **14.5 MB** | **0.91（用户态 −9%）** |
| 同上 + `target-cpu=native` | 21.7 | 36.0 | 14.6 MB | 1.09（**更差，弃用**） |

结论：
- **fat LTO + 1 CGU 采纳**（已写入 `Cargo.toml`），代价是 release 构建从 ~4 分 10 秒变成 ~8 分 45 秒。
- **`target-cpu=native` 明确有害**（用户态 +8.7%、内核态 +20%），不要加。这与直觉相反，但对 hyper/tokio 这种分支密集、函数指针多的代码路径，`-C target-cpu` 带来的指令选择变化可能破坏分支预测与代码布局。

**已经验证无效的**：mimalloc 参数调优（`PURGE_DELAY=-1`、`ARENA_EAGER_COMMIT=1`）——比值 0.308/0.293，和基线 0.328 无差异。

### 8.2 PGO（profile-guided optimization）：本轮最大单项收益

用 `scripts/pgo.ps1` 走完整三步：插桩编译（`-Cprofile-generate`）→ 在压测拓扑上跑真实负载采集 profile → `llvm-profdata merge` 后带 profile 重编译（`-Cprofile-use`）。Spire 本身已经有优雅停机（`SIGTERM` → `wait_for_signals` 正常返回），所以 LLVM 的 atexit 钩子能把 `.profraw` 正常落盘，**不需要改代码**。

同一会话内交替测量（`bin-ab.ps1`，3 轮中位数，1 核 c=64）：

| 构建 | 用户态 µs/req | 内核态 µs/req | 合计 | 体积 |
|---|---:|---:|---:|---:|
| gnu + fat LTO（基线） | 13.0 | 23.2 | 36.2 | 15.1 MB |
| **gnu + fat LTO + PGO** | **8.4（−35%）** | 21.9 | **30.3（−16%）** | 13.2 MB |

与 nginx 的同场次对比（`ab.ps1`）：

| 场景 | Spire（PGO） | nginx | CPU 比值 | 吞吐 |
|---|---:|---:|---:|---|
| 1 核 c=64 | 29.9 µs/req | 27.0 µs/req | **1.107×** | 33,711 vs 37,765 RPS（89%） |
| 4 核 c=64 | 39.4 µs/req | 30.3 µs/req | 1.298× | **83,100 vs 63,519 RPS（+31%）** |

**CPU/请求比值从摸高起点的 1.72 降到 1.11**，1 核吞吐从 59% 追到 89%，4 核已经反超。

### 8.3 一个必须记录的测量口径错误

前几轮的压测二进制是 **musl 静态**（`cross build --target x86_64-unknown-linux-musl`），但仓库 `build_and_deploy.bat` 实际发布的是 **gnu 目标**（`x86_64-unknown-linux-gnu`）。两者用户态差距明显（同源码下 musl 19.9 µs vs gnu 13.0 µs）：musl 的 syscall 包装与分配路径更重，而生产根本不用它。

**修正**：后续所有对比数据改用 gnu 目标（与生产一致）。§8.1 里"fat LTO −9%"的绝对值是在 musl 下测的，方向仍成立，但 gnu 下的绝对值更好。

---

## 9. 下一步优化清单（按预期收益排序，未实施）

目标很明确：**用户态 21.9 µs/req → 接近 nginx 的 6.7 µs/req**（内核态只差 19%，不是主战场）。

1. **绕开 hyper-util legacy client 的连接池（预期最大，估计 3~5 µs/req）**：每请求一次 `PoolInner` 的 DashMap 查找 + key 构造 + `PoolClient` future 链。nginx 把上游连接直接挂在客户端连接对象上，零池查找。做法：为 HTTP/1 上游自建「endpoint → `SendRequest`」缓存，用 `hyper::client::conn::http1::handshake` 直连。风险高，但只影响 HTTP/1 上游路径。
2. **合并 body 包装层（1~2 µs/req）**：一次响应现在要过 `BoxBody` → `TimedBody` → `PermitBody` → `BoxBody`，每层一次堆分配 + 虚调用；把 `TimedBody` 折叠进 `PermitBody`。
3. **指标句柄缓存（0.5~1.5 µs/req）**：每请求 5 次 `with_label_values`（RwLock + 哈希）。路由解析时取出 `Histogram`/`IntCounter` 存进路由结构，热路径只 `.observe()`/`.inc()`。
4. **`HeaderMap` 克隆收敛**：响应阶段仍有一次整表克隆（`cloned_headers`），可用 `Arc<HeaderMap>` 或借用化。
5. **生产环境层面**：`log_level` 固定 `info` 以上、logger 改 `lossy(true)` 或采样；Docker 网络若能改 host 网络/关 conntrack，可再吃掉一部分内核成本。

**已验证无效或收益很小的**：mimalloc 参数调优（`PURGE_DELAY=-1`、`ARENA_EAGER_COMMIT=1`）——比值 0.308/0.293 vs 基线 0.328，无差异；微观分配消除（改动 4-5）只带来约 1%，说明真正的差距在 hyper 的结构性开销上。

---

## 10. 局限性

1. WSL2 + Docker Desktop 不是裸机：cpuset 绑的是 vCPU，宿主仍有进程竞争；**绝对数字不可作为容量规划依据**。
2. 响应体固定 96 字节 = "纯转发开销"测试；大 body 场景下 `memcpy`/`writev` 占比会变。
3. 只测 HTTP/1.1 + keepalive + 无 TLS + 无 WASM；TLS(rustls)、HTTP/2、限流/熔断不在本轮。
4. nginx 侧只改 `worker_processes`（由 `nproc` 在 cpuset 内推导）与 `worker_rlimit_nofile`，其余与仓库配置一致。
5. 矩阵里 nginx 的行为单次测量（机器时间被压缩），Spire 1~2 核为 3 次中位数。
6. `_raw_spin_unlock_irqrestore` 这类内核符号在 `cpu-clock` 软件事件下归因偏粗，宜按"内核网络路径整体"读。
