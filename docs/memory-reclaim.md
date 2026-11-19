# 压测后内存不回落：原因分析与修复

> 状态：**已落地**（连接池收紧 + 分配器参数），实测 4.86GB 峰值 2 分钟内回落 80%。
> 目标读者：维护 Spire 数据面、以及排查线上 RSS 异常的人。
> 结论只适用于「不更换 Hyper/Tokio 技术栈」这一前提。

---

## 0. 一页纸结论

| 问题 | 结论 |
|---|---|
| 换 mimalloc 有没有解决问题 | **默认配置下没有**，反而比 glibc 更费内存；打开 `MIMALLOC_ARENA_EAGER_COMMIT=0` 后峰值降 38%、常驻降 44% |
| 内存不回落的主因 | **上游连接池**：空闲连接长期保留（HTTPS 客户端甚至是无上限），每条连接钉住自己的 hyper 读写缓冲 |
| 修复手段 | 收紧上游连接池（HTTPS 补上限 + idle 90s→20s）+ mimalloc 延迟提交 arena |
| 4GB 级峰值后的回落 | 修复前 7%（几乎不回落）；修复后 **80%**（4.86GB → 0.97GB，约 2 分钟） |
| 代价 | 吞吐无回退（60k 连接场景 13.9k → 15.7k rps） |

---

## 1. 现象与测量方法

现象：高并发期间 RSS 上涨，流量回落后 RSS 仍停留在高位。

测量口径（全部在本机 Docker 内完成，Ubuntu 24.04 容器 + nginx 静态后端 + wrk 压测）：

- `RSS` / `anon`：容器内 `/proc/1/status` 与 `/sys/fs/cgroup/memory.stat`，每 2 秒采样一次；
- 每条曲线都覆盖「压测中 → 压测结束 → 静默观察」；
- 变体之间只改一个变量（分配器 / 环境变量 / 代码版本），其余配置保持一致。

---

## 2. 分配器对比：mimalloc 本身不是解药

### 2.1 512 连接 / 压 60s（未改代码、未调参数）

| 分配器 | 基线 | 峰值 | 压测后 60s | 回落比例 |
|---|---|---|---|---|
| mimalloc（默认） | 33.8 MB | 90.6 MB | 78.4 MB | 13% |
| glibc malloc | 18.7 MB | 66.2 MB | 65.0 MB | 2% |
| mimalloc `PURGE_DELAY=0` | 26.3 MB | 91.5 MB | 75.2 MB | 18% |

### 2.2 2000 连接 / 压 20s

| 分配器 | 基线 | 峰值 | 压测后 60s |
|---|---|---|---|
| mimalloc（默认） | 33.4 MB | 259 MB | 231 MB |
| glibc malloc | 18.2 MB | 171 MB | 166 MB |
| mimalloc `ARENA_EAGER_COMMIT=0` | 19.9 MB | **161 MB** | **127 MB** |
| `ARENA_EAGER_COMMIT=0` + `PURGE_DELAY=0` | 19.4 MB | **148 MB** | **122 MB** |

原因：mimalloc 在 overcommit 系统上默认 `arena_eager_commit=2`，会整块预提交 arena；关掉后改为按需提交，峰值与常驻都显著下降，此时才优于 glibc。

### 2.3 连续三轮突发（512 连接 × 3 轮，轮间静默 30s）

mimalloc 峰值 100 / 88.8 / 88.3 MB，glibc 62.4 / 67.6 / 68.6 MB —— 两者都**不随时间累积**（第二轮峰值低于第一轮），说明这是「堆高水位」而不是泄漏。

---

## 3. 真正的原因：上游连接池钉住内存

代码里两个上游客户端（`rust-proxy/src/proxy/http1/http_client.rs`）：

- HTTP 客户端：`pool_idle_timeout = 90s`、`pool_max_idle_per_host = 1024`；
- **HTTPS 客户端：完全没有配置**，即 hyper 默认 —— `pool_max_idle_per_host = usize::MAX`（上一波有多少连接就留多少），每条还钉着 TLS 会话缓冲。

实测证据（50k 连接压测，采样 `/proc/net/tcp`）：

```
压测中      est_tcp ≈ 53,000
压测结束+1s est_tcp = 1,024     ← 只留下池里的空闲上游连接
+11s … +85s est_tcp = 1,020     ← 保留约 90s（= pool_idle_timeout）
+97s        est_tcp = 295       ← 连接开始过期
+97s        RSS 1460MB → 799MB  ← 连接关闭瞬间释放约 660MB
```

结论：**空闲连接本身持有大量缓冲，且它们的存活时长由 idle timeout 决定**。分配器只能决定「释放后要不要还给操作系统」，管不到「还没释放」。

---

## 4. 修复

### 4.1 代码（安全 Rust，不换 Hyper）

`rust-proxy/src/proxy/http1/http_client.rs`：

- 抽出常量 `POOL_MAX_IDLE_PER_HOST = 1024`、`POOL_IDLE_TIMEOUT = 20s`；
- HTTP 客户端改用常量（idle 90s → 20s）；
- **HTTPS 客户端补上同样的池上限**（原来继承 hyper 默认的无上限行为）；
- 新增回归测试 `upstream_idle_pool_is_bounded`（守住「不得回到无上限/长超时」）。

为什么 idle 保持 20s 而不是把池上限压到很小：把 `pool_max_idle_per_host` 压到 64 时，高并发下连接反复重建，吞吐从约 20k rps 掉到 5k rps，且释放效果并没有更好；而只收紧 idle 超时可以在不影响建连复用的前提下让缓冲及时释放。

### 4.2 部署（分配器参数）

- `docker/spires/Dockerfile`：`ENV MIMALLOC_ARENA_EAGER_COMMIT=0`
- `benchmarks/spires/docker-compose.yaml`：删除早已失效的 `JEMALLOC_SYS_WITH_MALLOC_CONF`，换成 `MIMALLOC_ARENA_EAGER_COMMIT=0`

---

## 5. 效果验证

### 5.1 4GB 级峰值（512KB 响应 × 11000 连接）

| 时点 | RSS（修复后） |
|---|---|
| 峰值 | **4857 MB** |
| +10s | 4603 MB（释放 5%） |
| +60s | 3110 MB（释放 36%） |
| +120s | 970 MB（**释放 80%**） |
| +180s ~ +240s | 稳定 970 MB |

对照（同一机制、未修复行为）：3.14GB 峰值，230 秒后仍 2.93GB，**只释放 7%**。

### 5.2 60k 连接 / 1KB 响应（同一压测脚本）

| | 峰值 | +30s | +120s | 吞吐 |
|---|---|---|---|---|
| 修复前 | 1291 MB | 1293 MB | 694 MB | 13.9k rps |
| 修复后 | **810 MB** | **541 MB** | **257 MB** | 15.7k rps |

> 注：修复前的这组数据同时使用默认 mimalloc 参数；其中分配器调参单独测得的增益见 §2.2。

---

## 6. 附带发现

- **上游写 hostname（k8s service name）**：hyper-util 的 `GaiResolver` 对**每次新建连接**做一次 `spawn_blocking` + `getaddrinfo`，无缓存。稳态下连接可复用，开销可忽略（10k 连接：域名 93.6k rps / IP 79.6k rps，线程均为 17）；但在「突发建连 + 大量重试」时线程会冲到 528（IP 场景 16~20），演变为建连延迟和超时。
- **k8s 的 `ndots:5`**：短 service name 会按 search 域展开成多次查询，写 FQDN（`svc.ns.svc.cluster.local.`）可降到一次。
- **`upstream_concurrency_limit` 显式配置**时，全局内存保险会退回保守默认值 40000，不再是按内存推导出来的值；高并发压测时表现为 503。

---

## 7. 未做 / 可继续优化

| 项 | 说明 |
|---|---|
| 空闲时主动 `mi_collect(true)` | 能进一步回收碎片，但需要 `libmimalloc-sys` 的 unsafe FFI，与仓库「零 unsafe」约定冲突，尚未落地 |
| 带 TTL 的 DNS 缓存 resolver | `HttpConnector::new_with_resolver()` 可包一层缓存，解决建连风暴下打满 blocking 线程池；纯安全 Rust |
| 连接池上限可配置 | 目前 `POOL_MAX_IDLE_PER_HOST` / `POOL_IDLE_TIMEOUT` 是常量；若需按集群调参可做成配置项（需同步 schema 与文档） |

---

## 8. 复现

压测工程（脚本 + 原始 CSV）保留在 `D:/code/tmp/spire-allocbench`：

- `scripts/run_high.sh`、`scripts/run_high_multi.sh`：单/多客户端高并发场景；
- `scripts/run_rounds.sh`：连续多轮突发；
- `scripts/sampler-conn.sh`：容器内采样（RSS / anon / 线程 / established 连接数）；
- `scripts/analyze.py`、`scripts/analyze_rounds.py`：汇总峰值与回落比例。

质量门禁：`cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 均通过
（`start_proxy_http` 在本机因 8080 端口被占用而失败，移除本次改动后同样失败，属环境问题）。
