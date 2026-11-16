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

**1）同核对比：优化前 Spire 约为 nginx 的 57%，优化后中位数约 84%（区间 61%~126%，受宿主噪声影响）。**

| 场景 | 优化前 | 优化后（交错 A/B） |
|---|---|---|
| 1 核 c=64 | spire/nginx ≈ 0.59 | 0.84 / 1.17（两次） |
| 4 核 c=64 | — | 0.77 / 0.61（两次） |
| 4 核 c=256 | — | **1.26**（Spire 反超） |

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

采样：4 核、c=256、47,235 RPS 稳定承压 50 秒，**160,559 个样本**，`results/flamegraphs/spire-4core-perf.svg`。自耗时由 `scripts/flamegraph_top.py` 从 SVG 的 `fg:x/fg:w` 重建调用树后计算（自耗时之和 = 根样本数，已校验）。

| 自耗时占比 | 符号 | 归因 |
|---:|---|---|
| **22.88%** | `_raw_spin_unlock_irqrestore` | 内核自旋锁（网络栈并发路径） |
| 4.94% | `memcpy` | 数据拷贝 |
| 2.47% | `__nf_conntrack_find_get` | Docker bridge conntrack 查表（**环境 overhead**） |
| 2.16% | `_mi_page_malloc_zero` | mimalloc 分配（每请求） |
| 1.87% | `spire::proxy::http1::http_proxy::proxy` | Spire 自身转发逻辑 |
| 1.52% | `mi_free` | mimalloc 释放（每请求） |
| 0.97% | `hyper::proto::h1::dispatch::Dispatcher::poll_loop` | HTTP/1.1 编解码 |
| 0.79% | `__inet_lookup_established` | 内核连接查表 |
| 0.72% | `nft_do_chain` | nftables（**环境 overhead**） |
| 0.59% | `selinux_file_permission` | SELinux 钩子（**环境 overhead**） |
| 0.51% | `prometheus ... with_label_values` | 指标打点（每请求） |

按组件：hyper 14.6%、tokio 5.4%、libc mem 5.3%、**mimalloc 3.7%**、内核发送路径 1.4%、内核接收 0.8%、epoll/唤醒 0.8%、tracing 0.7%。

**结论**：
- 每请求的**分配/释放**是 Spire 自己最大的一块（mimalloc 3.7% + memcpy 4.9%），`bytes::bytes_mut::shared_v_drop` 也进了前 20。
- 内核那段（自旋锁 + conntrack + nftables + SELinux）里有相当比例是 **Docker bridge 的环境成本**，生产上用 host 网络/eBPF 转发会低很多，**不要把这段算成 Spire 的账**。
- Spire 业务逻辑仅 ~3%，所以"Rust 写得慢"不是主因，**慢在每请求的固定开销**。

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
| 4 | pprof 采样可控（频率/时长/输出路径环境变量） | `app/run.rs` | 让火焰图能采到"稳态饱和点"而不是"启动 20 秒" |

**实测（同场次交错 A/B，`scripts/ab.ps1`）**

| 场景 | 改动前 | 改动后 |
|---|---:|---:|
| 1 核 c=64 | 0.328 / 0.559（spire/nginx 分别对 ceiling） | **0.838**（spire/nginx 直接比） |
| 4 核 c=256 | — | **1.256** |
| 4 核 c=64 | — | 0.768 / 0.612 |

说明：绝对值波动来自宿主噪声（nginx 自己在两次相同条件下跑出 14.7k 与 47k），所以只报比值，并且同一场景重复 2 次给出区间。**"持平 nginx"在 4 核 c=256 已达成，1~4 核 c=64 仍未稳定达成。**

**已经验证无效的**：mimalloc 参数调优（`PURGE_DELAY=-1`、`ARENA_EAGER_COMMIT=1`）——比值 0.308/0.293，和基线 0.328 无差异。

---

## 9. 下一步优化清单（按预期收益排序，未实施）

1. **缓存指标句柄**：`http_proxy.rs:243/249` 每请求两次 `with_label_values`（RwLock + 哈希查找 + key 分配）。改成在路由解析时把 `Histogram`/`IntCounter` 取出来存进路由结构，热路径只 `.observe()`/`.inc()`。预计 2~5%。
2. **去掉每请求的 `HeaderMap` 克隆与 `path.to_string()`**：`proxy()` 里 `req.headers().clone()`、`uri`/`method` 克隆、`path` 字符串化只为错误日志服务；改成借用 + 懒生成。
3. **上游超时可关闭**：`http_client.rs` 的 `timeout()` 每请求注册一个 tokio 定时器；当路由未配置超时时可以完全跳过（需要把返回类型从 `Timeout<ResponseFuture>` 改成枚举/`BoxFuture`）。
4. **`writev` 合并**：确认 hyper 是否把 head 与 body 合并成一次 `writev`；若否，可用 `http1::Builder::max_buf_size` + 一次性 `Bytes` 减少一次系统调用。
5. **生产环境层面**：把 `log_level` 固定为 `info` 以上、logger 改 `lossy(true)` 或采样；Docker 网络若可改 host 网络/关 conntrack，能直接吃掉火焰图里 3%+ 的环境 overhead。

---

## 10. 局限性

1. WSL2 + Docker Desktop 不是裸机：cpuset 绑的是 vCPU，宿主仍有进程竞争；**绝对数字不可作为容量规划依据**。
2. 响应体固定 96 字节 = "纯转发开销"测试；大 body 场景下 `memcpy`/`writev` 占比会变。
3. 只测 HTTP/1.1 + keepalive + 无 TLS + 无 WASM；TLS(rustls)、HTTP/2、限流/熔断不在本轮。
4. nginx 侧只改 `worker_processes`（由 `nproc` 在 cpuset 内推导）与 `worker_rlimit_nofile`，其余与仓库配置一致。
5. 矩阵里 nginx 的行为单次测量（机器时间被压缩），Spire 1~2 核为 3 次中位数。
6. `_raw_spin_unlock_irqrestore` 这类内核符号在 `cpu-clock` 软件事件下归因偏粗，宜按"内核网络路径整体"读。
