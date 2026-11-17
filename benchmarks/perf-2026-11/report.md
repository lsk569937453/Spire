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

**原始数据可复核性**：`results/` 下的 CSV 都可以被标准 CSV 解析器直接读（`cpuset` 含逗号，已加引号；数字不用千分位）。2026-11-17 做过一次数据修复：早期脚本用字符串拼接写 CSV，`cpuset` 的逗号与本机 locale 的千分位分隔符会让每一列错位，且中途更换过表头语义（早期是"对 ceiling 的比值"，后期是"µs/req + CPU 比值"）却没更新表头。修复后的 `ab.csv` 是统一 schema（µs/req + CPU 比值 + 吞吐比），早期那份原始日志保留为 `results/ab-legacy-log.txt`（仅供人眼追溯，不要当 CSV 解析）。四个写出脚本（`ab.ps1`/`bin-ab.ps1`/`quick.ps1`/`exp.ps1`）已改为 `Export-Csv` + `F` 格式。

**一键复核本文所有引用数字**：

```sh
python scripts/verify_report.py   # 40 项检查，全部来自 results/*.csv，退出码非 0 表示报告与数据脱节
```

---

## 1. 结论摘要

**1）同核对比（每格 = 3 次同场次交错测量的中位数）：2 核 +14%、4 核 +18%，1 核 85%。**

最终构建 = gnu 目标 + `lto="fat"` + `codegen-units=1` + PGO（4 核 c=64 采样），镜像 `perf-spire:pgo3`。与 nginx 同场次交错测量（`ab.ps1`，20 秒/档）：

| 核数 | Spire CPU/req | nginx CPU/req | CPU 比值 | Spire RPS | nginx RPS | 吞吐比 |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 30.7 µs | 25.6 µs | 1.199× | 33,554 | 39,297 | 0.85 |
| 2 | 30.0 µs | 27.0 µs | **1.111×** | 62,886 | 55,290 | **1.14** |
| 4 | 37.8 µs | 31.4 µs | 1.199× | 88,400 | 74,909 | **1.18** |

三档各 3 次重复的**逐次原始值**（与 `results/ab.csv` 一致，可复核）：

```
1 核 CPU 比值 1.197 / 1.142 / 1.236      吞吐 0.869 / 0.874 / 0.822
2 核 CPU 比值 1.100 / 1.121 / 1.077      吞吐 1.137 / 1.131 / 1.186
4 核 CPU 比值 1.237 / 1.190 / 1.199      吞吐 1.180 / 1.139 / 1.281
```

**结论**：2 核与 4 核的吞吐**稳定超过 nginx**（+14%、+18%，三轮全部为正）；1 核稳定在 **0.85**。1 核的 CPU/请求在 1.14~1.24 之间，且 Spire 的系统调用次数比 nginx 更少（3.92 vs 5.15 次/请求）——剩余差距在依赖库内部（hyper 占用户态 12~15%），不是"我们多做系统调用"或"漏了某个优化"。

摸高轨迹（CPU/请求，vs nginx）：**1.72 → 1.49（fat LTO）→ 1.47（endpoint_key）→ 1.11（PGO）→ 1.11~1.20（三档稳定区间）**。

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
| 9 | **并发上限 gauge 只在解析时导出一次** | `proxy/concurrency.rs` | 它是每条路由的常量，却每请求重写一次；改成线程本地解析时导出，1 核总 CPU 35.5 → 34.4 µs（−3%，主要落在内核态 22.9 → 21.6 µs） |

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

**CPU/请求比值从摸高起点的 1.72 降到 1.11**，1 核吞吐从 59% 追到 89%，2 核已反超 8%。

**profile 训练集的选取（两个反直觉结论）**：

1. 用 4 核 c=64 采的 profile 与"4 核 + 1 核混合训练"比：**混合训练差 5.5%**（用户态 8.66 → 9.14 µs），保留单一 4 核训练集。
2. 改了源码（那时还尝试移除 tokio 的 `tracing` feature）之后**重新采 profile 重建，结果反而比旧 profile 差 4.3%**（用户态 8.3 → 8.6 µs）。**PGO 的收益对"用哪份 profile"很敏感**，换 profile 之前必须做同会话 A/B，不能假设"profile 越新越好"。

同时验证并**回退**的一项：tokio `features = ["full", "tracing"]` 里的 `tracing`。纯用户态火焰图里 `tokio::task::raw::poll` 的 instrument 包装占 1.17%、`tracing_subscriber::Layered` 占 1.09%，看着像白送的收益；但同会话 A/B 只有 1.2%（噪声内），并在重采 profile 后整体劣化，因此保留原样——**剖面里的符号占比不等于可回收的收益**。

### 8.4 每请求全局锁的消除（线程本地缓存）

`limiter_for()` 每请求都要对进程级 `RwLock<HashMap>` 做一次读锁 + 哈希查找，`endpoint_key()` 还要分配一次 `String`。改成线程本地缓存（记录最近一次的 `endpoint → key → limiter → cfg`，命中时只是字符串比较 + 两次 `Arc` 自增）：

| 场景 | 优化前用户态 | 优化后 | 变化 |
|---|---:|---:|---:|
| 1 核 c=64（普通 release） | 11.8 µs | **11.0 µs** | **−6.2%** |
| 4 核 c=64（普通 release） | 15.7 µs | 15.7 µs | 0% |
| 1 核 c=64（PGO 构建） | 8.9 µs | 9.0 µs | 噪声内（PGO 已把这部分成本吃掉） |

**1 核有效、4 核无效**——这否定了"4 核恶化是锁竞争"的假设，4 核的 CPU/请求上涨（29.9 → 39.4 µs）更可能来自多核下的缓存/频率效应，而不是我们代码里的锁。

### 8.5 一个被数据否掉的改动（记录以免重复踩）

跳过 `handle_before_response` 的整表 `HeaderMap` 克隆（无中间件时不必克隆）：同会话 A/B **1.6%（噪声内，且略差）**，已回退。结论：**每请求的分配次数已经不是瓶颈了**（~15 次分配 × 50ns ≈ 0.75µs，占 11µs 用户态的 6.6%，删掉 1 次属于噪声量级）。要继续压缩用户态，只能动 hyper 的结构（它占 12~15%）或内核路径，属于重构级工作。

### 8.7 负结果清单（都已回退，记录以免后人重复尝试）

| 尝试 | 预期 | 实测 | 结论 |
|---|---|---|---|
| `target-cpu=native` | 指令级优化 | 用户态 +8.7%、内核态 +20% | 有害，弃用 |
| mimalloc 参数（`PURGE_DELAY=-1` / `ARENA_EAGER_COMMIT=1`） | 减少页回收 | 0.308/0.293 vs 基线 0.328 | 无差异 |
| 换成 glibc malloc（去掉 mimalloc 全局分配器） | 小对象更快 | 1 核持平（0.996），**4 核差 8.1%** | 保留 mimalloc |
| 移除 tokio 的 `tracing` feature | 剖面里占 2.3% | 1.2%（噪声内），重训 PGO 后更差 | 回退 |
| 跳过 `HeaderMap` 整表克隆 | 少一次分配+拷贝 | 1.6%（噪声内，略差） | 回退 |
| 混合训练集（1 核 + 4 核 profile） | 兼顾两种负载 | 比纯 4 核 profile **差 5.5%** | 保留单训练集 |
| 线程本地 limiter 缓存 | 去全局锁 | 普通构建 −6.2%，PGO 构建噪声内，4 核 0% | 保留（逻辑更省，未回退） |
| 自管 HTTP/1 上游连接（`http1::handshake` + 每连接缓存 `SendRequest`） | 火焰图里 hyper-util client 占 1.9% | **设计上否决**：请求体是流式 `BoxBody`，连接空闲被对端关闭后无法安全重试，只能给客户端 502——为 2~4% 的收益引入生产回归风险，不值得 |

这七条加起来说明：**剩下的差距已经不在"我们能改的边角"里**。1 核要再往前，只能换掉 hyper 的上游客户端或改造请求/响应缓冲路径，属于重构级工作，风险与收益不对称。

### 8.8 一个必须记录的测量口径错误

前几轮的压测二进制是 **musl 静态**（`cross build --target x86_64-unknown-linux-musl`），但仓库 `build_and_deploy.bat` 实际发布的是 **gnu 目标**（`x86_64-unknown-linux-gnu`）。两者用户态差距明显（同源码下 musl 19.9 µs vs gnu 13.0 µs）：musl 的 syscall 包装与分配路径更重，而生产根本不用它。

**修正**：后续所有对比数据改用 gnu 目标（与生产一致）。§8.1 里"fat LTO −9%"的绝对值是在 musl 下测的，方向仍成立，但 gnu 下的绝对值更好。

---

## 9. 决策记录：为什么这次不改内核（含 Pingora 评估）

1 核的剩余差距（CPU/请求 1.14~1.24，吞吐 0.85）已经不在"可微调"的层面。**2026-11-17 决定：本轮到此为止，不启动内核级重写**，理由与备选路线记录在此，供将来立项时直接复用。

### 9.1 仍是结构性选项、但本次不做

| 选项 | 预期收益 | 代价/风险 |
|---|---|---|
| 自管 HTTP/1 上游连接（`hyper::client::conn::http1::handshake` + 每下游连接缓存 `SendRequest`） | 2~4%（火焰图里 hyper-util client 占 1.9%） | **已否决**：请求体是流式 `BoxBody`，连接空闲被对端关闭后无法安全重试，只能给客户端 502 |
| 合并 body 包装层（`TimedBody` 折进 `PermitBody`，少 2 次 `BoxBody`） | 1~2% | 触及并发 permit 生命周期，需重跑限流/超时相关测试 |
| 指标句柄缓存（路由解析时取出 `Histogram`/`IntCounter`） | 0.5~1.5% | 引入每路由缓存，配置热重载时要失效 |
| 保留 hyper、只改请求/响应缓冲路径（header 零拷贝直通、减少 `Bytes` 中转） | 个位数 % | 改 `forward_http_upstream` 与 body 链；**这是唯一还值得试的一档** |

### 9.2 换框架（Pingora）评估

结论：**为"单核追平 nginx"换 Pingora 不划算**，理由如下（均为分析，未在本机实测 Pingora 的 µs/req）。

1. **它解决的不是我们量到的瓶颈**。Pingora 的 HTTP/1 与 HTTP/2 实现在 `h1` / `h2` crate 上，`h1` 本身源自 hyper 的解析器——每请求仍有 header 解析、类型化 Request/Response 构造、响应再序列化。我们的差距正落在这一段（hyper 占用户态 12~15%、`memcpy` 9.8%），换框架不会自动消失。
2. **Pingora 的优势在连接生命周期**：连接复用与健康检查、优雅升级、多协议统一、TLS 会话与服务发现。这些是生产可用性收益，不是单请求 CPU 收益。
3. **迁移面是全站级的**。Spire 的中间件、熔断、限流、JWT、CORS、WASM 插件、指标导出都挂在 hyper 的 `Request`/`Response` 上（`middleware/`、`proxy/proxy_trait.rs`、`wasm/`、`monitor/prometheus_exporter.rs`）；`PermitBody`/`TimedBody` 的并发预算与超时语义也要重新落到 Pingora 的 body 模型；gRPC 转码与 WebSocket upgrade 需重新验证。这是换内核，不是优化。
4. **依赖成本**：Pingora 会引入 BoringSSL/OpenSSL 等重依赖，与现在 rustls 的技术栈不一致。

### 9.3 将来若要立项：先跑 spike，用数据而不是判断

在动主干之前，先做 1~2 小时的 spike：用 Pingora 写一个最小 echo 代理（或最小 `h1` 连接循环），放进**现有 rig**（同一 bridge、同一 cpuset、`bin-ab.ps1` 同会话交替、读 `/proc` 的 utime/stime）与 `perf-spire:pgo4` 对比。

**立项门槛**：Pingora/h1 版本的 µs/req 明显低于当前 1 核的 30.7 µs（建议门槛 ≤26 µs，即 ≥15% 改善），否则直接放弃。

### 9.4 生产环境层面的零代码收益

- `log_level` 必须固定在 `info` 以上：`debug` 会打开访问日志，4 核吞吐 **−49%**（§5）。
- Docker 网络若能改 host 网络或关闭 conntrack，可再吃掉一部分内核成本（火焰图里 conntrack/nftables/SELinux 合计 ~4%）。
- 发布二进制请用 **gnu 目标 + fat LTO**（生产脚本本来就是 gnu）；PGO 见 §8.2 与 `scripts/pgo.ps1`。

**已经确认无效的八项**见 §8.7。

---

## 10. 局限性

1. WSL2 + Docker Desktop 不是裸机：cpuset 绑的是 vCPU，宿主仍有进程竞争；**绝对数字不可作为容量规划依据**。
2. 响应体固定 96 字节 = "纯转发开销"测试；大 body 场景下 `memcpy`/`writev` 占比会变。
3. 只测 HTTP/1.1 + keepalive + 无 TLS + 无 WASM；TLS(rustls)、HTTP/2、限流/熔断不在本轮。
4. nginx 侧只改 `worker_processes`（由 `nproc` 在 cpuset 内推导）与 `worker_rlimit_nofile`，其余与仓库配置一致。
5. 矩阵里 nginx 的行为单次测量（机器时间被压缩），Spire 1~2 核为 3 次中位数。
6. `_raw_spin_unlock_irqrestore` 这类内核符号在 `cpu-clock` 软件事件下归因偏粗，宜按"内核网络路径整体"读。
