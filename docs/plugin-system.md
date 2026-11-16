# Spire 的 WebAssembly 插件系统设计

> 状态：**已落地 M1**（headers 阶段 + 全链路配置/隔离/可观测/热重载 + 真实 guest 端到端测试）。
> 目标读者：维护 Spire 数据面的人，以及要写插件的第三方。
> 参考实现：`rust-proxy/src/wasm/`（宿主）、`rust-proxy/wit/spire-filter.wit`（ABI）、
> `rust-proxy/plugins/header-annotator/`（guest 示例）。

---

## 0. 一页纸结论

| 问题 | 结论 |
|---|---|
| 用什么做沙箱 | wasmtime 50 + **Component Model**（WIT 类型化 ABI），不用 proxy-wasm 的线性内存 ABI |
| 挂在哪里 | 复用现有 `MiddleWares` 中间件链，新增 `kind: wasm`，**不改动**路由/健康检查/并发限流任何代码 |
| 每请求开销 | 路由没有配 `kind: wasm` 时 **0 额外开销**（只有一次 `matches!`）；配了时是 3 次同步 wasm 调用 + 头部 map 的一次复制 |
| 实例模型 | **每 worker 线程 × (插件, 配置) 一个实例**，无锁、无 channel 跳转 |
| 隔离手段 | fuel（确定性 CPU）+ epoch 截止时间（墙钟）+ 线性内存上限 + 输出长度上限 + 能力白名单 |
| 崩溃语义 | trap 后**丢弃实例**，按路由 `fail: open/closed` 决定放行还是 503 |
| 为什么不直接上 proxy-wasm | 见 §2.3：ABI 兼容债、header 单值读取缺陷、无内存/CPU 硬上限 |

---

## 1. 现状：Spire 的中间件管线（改造前的真实形态）

Spire 原本没有插件机制，中间件是一个**封闭枚举**：

```rust
// rust-proxy/src/middleware/middlewares.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum MiddleWares {
    #[serde(rename = "rate_limit")]       RateLimit(Arc<Mutex<Ratelimit>>),
    #[serde(rename = "authentication")]   Authentication(Authentication),
    #[serde(rename = "allow_deny_list")]  AllowDenyList(AllowDenyIp),
    #[serde(rename = "cors")]             Cors(CorsConfig),
    #[serde(rename = "rewrite_headers")]  Headers(StaticResourceHeaders),
    #[serde(rename = "forward_headers")]  ForwardHeader(ForwardHeader),
    #[serde(rename = "circuit_breaker")]  CircuitBreaker(Arc<Mutex<CircuitBreaker>>),
    #[serde(rename = "ip_ban")]           IpBan(Arc<Mutex<IpBan>>),
    #[serde(rename = "request_headers")]  RequestHeaders(RequestHeaders),
    #[serde(rename = "compression")]      Compression(Compression),
    // ← 新增
    #[serde(rename = "wasm")]             Wasm(WasmPluginConfig),
}
```

`Middleware` trait 提供三个钩子，管线在四个位置调用它们：

| 阶段 | 钩子 | 调用点 | 能拿到什么 |
|---|---|---|---|
| 路由准入 | `check_request(&SocketAddr, Option<&HeaderMap>)` | `RouteConfig::is_allowed`（`vojo/app_config.rs`），由 `ChainTrait::get_destination` 调用 | 只能看，不能改 |
| 请求头 | `handle_request(SocketAddr, &mut HeaderMap)` | `apply_request_middlewares`（`proxy/http1/http_proxy.rs`） | 只能改 header |
| 响应 | `handle_response(&str, &mut Response<BoxBody>, HeaderMap)` | `ChainTrait::handle_before_response`（`proxy/proxy_trait.rs`） | 整个响应 |
| 结果归档 | `record_outcome(&Result<Response, AppError>)` | 同上 | 只读 |

**三个必须正视的结构性事实**：

1. **配置即运行时对象**。`Vec<MiddleWares>` 直接挂在 `RouteConfig` 里，随 `AppConfig` 一起
   被 `Arc<RwLock<AppConfig>>` 持有、序列化回 YAML、通过热重载整体替换。
   所以新的 `Wasm` 变体里**只能放纯配置**，不能放编译产物、`Store`、连接池之类的东西
   （`Store` 是 `!Sync`，塞进 `AppConfig` 会直接编译失败）。这一条决定了整个架构的骨架：
   **变体存配置 → 运行时由一个进程级注册表按 id + 配置哈希解析出组件与实例**。
2. **请求路径上有两个"同步、无 body"的位置**和两个"有 body"的位置（HTTP/gRPC），
   body 是**流式**的（`upstream_body.rs` 的 `TimedBody`/`PermitBody`）。
   因此 M1 只接管 headers/trailers，body 阶段在 ABI 里先定好、宿主暂不调用（§8 说明取舍）。
3. **热重载是"整体替换 `AppConfig`"**，监听器不变。插件必须能跟着配置一起换代，
   且不能让正在处理的请求看到半旧半新的状态 → 用**代际（generation）**做实例键。

---

## 2. 两个参考工程：到底该学什么

### 2.1 openidea（Rust + Tauri IDE，wasmtime + Component Model）

事实要点（均取自源码）：

* **一个包多个 world**：`package openidea:plugin@1.9.0`，基础 `world plugin` + 十几个
  `world plugin-syntax { include plugin; export syntax-provider; }`。guest 的 SDK 宏把
  基础 world 和若干个"只导出"的 fragment world 合成同一个组件
  （`sdk/rust/openidea-plugin/src/lib.rs:271-284`）。
  → **收益**：宿主只认最小 world，插件按需多导出；组件模型允许"导出超集"仍然满足 world。
* **能力即授权**：所有宿主函数按 manifest 的 `[capabilities] required/optional` 门控，
  未授权返回 `error::denied(cap)` 而**不是 trap**（`crates/ide-plugin/src/wit_host.rs:263-266`）。
  → **收益**：插件可以自己降级，不会因为少一个权限就让整个路由挂掉。
* **引擎与预算**：`Config::new().wasm_component_model(true).consume_fuel(true).epoch_interruption(true)`，
  单独一条 10ms tick 线程 `increment_epoch()`（`crates/ide-plugin/src/runtime.rs:620-649`）；
  内存上限靠 `StoreLimitsBuilder`；单次调用超时/取消靠 `store.epoch_deadline_callback`
  （`runtime.rs:1121-1130`）。
* **实例模型：每个活跃插件一个 `std::thread` + `mpsc` 提交任务**，理由写在注释里：
  wasmtime 的 async API 需要全链路 async host binding，而"同一实例同一时刻只被一个任务进入"
  时，每插件一个线程代价很小（`crates/ide-plugin/src/actor.rs:10-13`）。
* **trap 与 guest 错误分流**：`CallError::{Guest, Trap}`——前者实例还能继续用，后者必须丢弃
  （`runtime.rs:900-908`）。
* **编译缓存**：`{blake3(bytes) + engine.precompile_compatibility_hash()}.cwasm`，
  读缓存后仍然 `Component::deserialize` 校验，坏文件删除重编（`runtime.rs:665-704`）。
* **热重载是手动的**：`reload()` 只重读目录，不监听文件、不触发 cargo（`ide-host/src/plugins/mod.rs:878-886`）。

**对网关的移植判断**：

| 采纳 | 理由 |
|---|---|
| WIT 多 world + 版本化包名 | 已采纳：`spire:filter@0.1.0`，`filter-plugin` world |
| capability 门控 + `denied` 而非 trap | 已采纳：`StoreData::require()` |
| fuel + epoch + StoreLimits 三件套 | 已采纳（tick 改成 1ms，见 §5.3） |
| trap 后丢弃实例 | 已采纳：`pool::discard()` |
| 编译期一次 + 缓存 | 编译期一次已采纳；`.cwasm` 缓存列为 M2 |

| 不采纳 | 理由 |
|---|---|
| 每插件一个常驻线程 + mpsc | 网关的请求已经在 N 个 tokio worker 上，再加一层线程跳转是纯损耗；且 IDE 的调用频率远低于网关。改用**线程本地实例池**（§5.2） |
| 手动 reload | 网关在 `SIGHUP` 热重载时重读配置并重编译插件目录，插件换代必须跟配置换代绑定 |
| 把 WASI 只当"时钟+随机"却仍全量链接 | 见 §5.1 的说明：Rust guest 必须链接 wasi-p2 才能编译，能做的只是把 ctx 配成 deny-all |

### 2.2 Envoy（C++ 代理，WASM HTTP filter + proxy-wasm）

事实要点：

* **扩展 = 工厂注册 + 配置 proto**，编译期用宏塞进全局 `FactoryRegistry`
  （`envoy/registry/registry.h:623-627`），运行时按名字查表。
  → 启发的不是"照抄注册表"，而是**名字寻址**：配置只写名字，不写实现。
  Spire 的 `plugin: io.example.header-annotator` 同理，加载期就校验，写错名字启动即失败。
* **过滤器状态机**：`FilterHeadersStatus` 有 5 个值，`StopIteration` 与 `continueDecoding()`
  严格配对，`allowIteration()` 里有 `ASSERT(iteration_state_ != Continue)`
  （`source/common/http/filter_manager.h:189-192`）。
  → **这是最重要的教训**：一旦 ABI 允许"暂停/恢复"，就必须定义清楚谁恢复、何时恢复，
  否则状态机会腐化。Envoy 的一致性来自注释即规格 + 断言。
* **头阶段默认禁止 `StopIteration`**（`source/extensions/common/wasm/context.cc:74-77`），
  需要额外开关；单值 header 读取只返回第一个值，作者自认是潜在安全问题
  （`context.cc:732-735`，注释 "This is a potential security issue"）。
* **VM key / plugin key 是手工维护的字段清单**，注释明说新增字段必须同步
  （`source/extensions/common/wasm/wasm.cc:402-415` 的 "Ideally, all fields ... should be part of the vm_key"）。
* **失败策略**：`FAIL_RELOAD` / `FAIL_CLOSED`(503) / `FAIL_OPEN`，默认闭；
  fail-open 的实现就是"不注册 filter"，fail-closed 是"注册一个空 Context"
  （`wasm.cc:710-729`）。
* **无内存/CPU 硬上限**：`VmConfig` 里根本没有这些字段，内存只被观测上报（`wasm.cc:149-158`）。
  隔离靠 sandbox，不靠配额。
* **能力限制挂在 VM 上**（`api/envoy/extensions/wasm/v3/wasm.proto:141-145`），
  同 VM 内所有插件共享同一个白名单。

**结论**：Envoy 的**配置/工厂思路**值得学，**运行时的资源治理不值得学**（它自己都把 WASM filter
标成 alpha + `security_posture: unknown`）。Spire 的做法是"Envoy 的配置形状 + openidea 的资源治理"。

### 2.3 为什么不用 proxy-wasm ABI

| proxy-wasm | 本设计 |
|---|---|
| C 风格 `proxy_*` 导出，宿主手写两段式内存写入 | WIT + canonical ABI，类型由组件模型校验 |
| header map 是"宿主侧句柄 + 索引"，单值读取只取第一个（已知缺陷） | 传 `list<header>` 进、返回 `list<header-op>` 出，语义无隐含状态 |
| `StopIteration`/`continueDecoding` 隐式状态机 | 无暂停语义：`proceed / reply / end` 三值决策，宿主一次应用 |
| 无 CPU/内存硬限（Envoy 现状） | fuel + epoch + `StoreLimits` |
| 只能二选一已实现版本，兼容靠宿主探测 ABI 版本 | 包名带上版本 `spire:filter@0.1.0`，加载期 reject 不兼容组件 |

代价：生态。proxy-wasm 有现成的 Go/TinyGo/JS SDK。本设计目前只有 Rust。这是刻意的取舍——
一个网关的插件 ABI 一旦发出去就很难改，宁可先小后大。

---

## 3. 目标与非目标

**目标**

1. 第三方能在**不重新编译网关**、**不重启进程**的前提下给某个路由加 L7 逻辑。
2. 插件出错不能把网关带下水：CPU、内存、时间、输出体积都要有硬上限。
3. 没有插件时零开销；有插件时单次调用在**微秒级**。
4. 配置写错在**加载期**报错（`spire validate` / 启动 / `SIGHUP` 重载），而不是在数据面每请求报错。
5. 可观测：插件调用次数/耗时/trap 原因、插件自定义指标，全部进 `/metrics`。

**非目标（M1 明确不做）**

* 不改写请求/响应 **body**（ABI 已定义，宿主 M2 才调用）。
* 不允许插件直接发起**上游 HTTP 调用**（外部鉴权类需求，M2 用异步桥实现）。
* 不做插件市场/签名（M3）。
* 不接管 TCP 代理路径与 gRPC 转码路径的 body。
* 不提供"单实例全局"模式（Envoy 的 `WasmService`）：那需要跨线程调度，M2 再加。

---

## 4. 架构总览

```
plugins 目录                                 路由配置
~/.spire/plugins/                            middlewares:
  io.example.header-annotator/                 - kind: wasm
    plugin.toml                                  plugin: io.example.header-annotator
    dist/plugin.wasm                             config: |
                                                   header=x-served-by
        │                                        fail: open
        │ scan + 校验 manifest                    timeout_ms: 5
        ▼                                        fuel: 2000000
  PluginPackage ──compile──► Component  ←──────────┘
        (进程级一次)             │
                                 │ 首次被某线程触达时 instantiate
                                 ▼
                    ┌────────────────────────────┐
                    │ 线程本地实例池 (TLS)        │
                    │ key = id # generation # cfg │
                    └────────────────────────────┘
                                 │
     lifecycle.start ─► lifecycle.configure ─► request_filter.on-request-headers
                                             ► response_filter.on-response-headers
                                             ► response_filter.on-log
```

模块划分（`rust-proxy/src/wasm/`）：

| 文件 | 职责 |
|---|---|
| `mod.rs` | 注册表：扫描、编译、代际、`resolve()`、四个钩子的编排、失败策略 |
| `manifest.rs` | `plugin.toml` 解析与校验（id/semver/api 兼容/路径逃逸/能力白名单） |
| `engine.rs` | `Engine` + `Linker`（唯一可导入集合）、预算装配、trap 分类 |
| `state.rs` | `StoreData`、`host::Host` 实现（能力门控）、共享状态、插件自定义指标 |
| `pool.rs` | 每线程实例池、淘汰、`lifecycle.stop` |
| `convert.rs` | `HeaderMap` ↔ WIT record，决策 → `Denial` |
| `bindings.rs` | `wasmtime::component::bindgen!`，编译期校验 WIT |

配置表面（**不依赖 wasmtime**）单独在 `src/middleware/wasm_plugin_config.rs`，
`src/middleware/wasm_plugin.rs` 做特征开关的桥接。这样默认构建仍然能**解析并拒绝**
`kind: wasm`，而不是静默忽略——见 §7.3。

---

## 5. 运行模型

### 5.1 沙箱

* `Engine`：`wasm_component_model(true)`、`consume_fuel(true)`、`epoch_interruption(true)`、
  `max_wasm_stack(512 KiB)`、`parallel_compilation(true)`。
* `Linker` 里**只有两个东西**：`spire:filter/host`（我们自己实现）+ WASI p2。
  WASI 之所以必须链接：Rust guest 编译到 `wasm32-wasip2` 时 std 会 import 它。
  但 `WasiCtx::builder()` 的默认值已经是 deny-all——stdin 关闭、stdout/stderr 丢弃、
  无环境变量、无参数、无 preopen、socket 全拒。**结果**：guest 有时钟和随机数，别的都没有。
* 没有 `http_call`、没有 `fs`、没有 `env`。想加必须改 WIT + 加能力 + 加实现，三处都改才生效。

### 5.2 实例模型：为什么是"每线程一实例"

约束来自类型系统：wasmtime 的 `Store<T>` 是 `!Sync`，每个回调都要 `&mut Store`。
任何"共享"方案都只能是加锁或加 channel —— 在网关热路径上是不可接受的。

所以：`thread_local! { HashMap<key, Rc<RefCell<Slot>>> }`，`key = "<plugin id>#<generation>#<配置哈希>"`。

* 请求在哪个 worker 线程处理，就用哪个线程上的实例，**零同步**。
* 一个线程第一次用到某插件时才 instantiate（+`start`+`configure`），之后复用。
* 池上限 `MAX_SLOTS_PER_THREAD = 64`，超过时按 `last_used` 淘汰最旧且空闲 >300s 的；
  淘汰时调用 `lifecycle.stop`。
* 配置变了 → `instance_key()` 变化 → 新实例；旧代际被 `retain_generation` 回收
  （当前线程立即回收，其它线程靠池上限懒淘汰）。
* 重载时 `generation` 自增，**在飞请求仍然持有旧 `Arc<CompiledPlugin>`**，不会看到半新半旧。

代价（必须写在明处）：内存上界 = `线程数 × Σ_插件(每线程实例数 × max_memory_bytes)`。
8 线程 / 3 插件 / 默认 16 MiB ⇒ 最坏 384 MiB；实际远小于此，因为 wasm 线性内存按需增长，
且只有真正被路由引用的插件才会实例化。M2 可以加"全局实例数上限"做软约束。

### 5.3 预算

一次回调的预算是**双保险**，缺一不可：

| 机制 | 值（默认） | 拦住什么 |
|---|---|---|
| `set_fuel(fuel)` | 2,000,000 | 死循环。确定性，负载无关 |
| `set_epoch_deadline(1)` + 1ms tick + `epoch_deadline_callback` | `timeout_ms` = 5ms | guest 在宿主调用里卡住、或机器本身慢 |
| `StoreLimitsBuilder::memory_size` | 16 MiB | 内存炸弹。`trap_on_grow_failure(true)` |
| `max_body_bytes` / `max_output_bytes` | 1 MiB / 64 KiB | 一次回调返回的超大 body/头部 |
| `LOG_BUDGET` | 64 行/次 | `host.log` 刷日志 |
| 共享状态上限 | 1024 键 / 4 MiB / 单值 256 KiB | 插件把网关当数据库 |

instantiate 用单独的 `INSTANTIATION_BUDGET = 500ms`：它不是请求预算，
否则 5ms 的请求预算会在实例化还没结束时就杀掉实例（这个 bug 在实现时真实出现过，
见 `engine.rs` 的注释）。

### 5.4 失败语义

| 情况 | 行为 |
|---|---|
| guest 返回 `reply` | 直接作为本地响应返回，不再走响应中间件（与路由拒绝一致） |
| guest trap / fuel 耗尽 / 超时 | 丢弃实例 → 按 `fail` 策略：`open` 记日志+指标后放行，`closed` 返回 503 |
| 插件未加载 / 配置非法 | `AppError` → 502（这是运维/配置错误，不是请求错误） |
| `on-log` 失败 | 只记日志，响应已提交，不影响客户端 |
| 插件返回非法 header 名/值 | 视为插件错误，走 `fail` 策略（不会把畸形头透传给上游） |

**`fail` 默认 `open`**：一个只做注解/统计的插件坏掉不该让业务挂掉。
但鉴权/WAF 类插件必须显式写 `fail: closed` —— 这一点写在示例配置的注释里，
因为它是最容易配错、后果最严重的开关。

---

## 6. ABI（`wit/spire-filter.wit`）

设计要点：

1. **只有两个方向的回调 + 一个生命周期**，不做"可选导出"的碎片 world（M2 需要时再加，
   组件模型允许导出超集，加 world 不需要破坏兼容）。
2. **决策是数据，不是副作用**：guest 返回 `headers-decision { proceed(list<header-op>) | reply(local-reply) }`，
   宿主负责应用。guest 永远拿不到网关内存。
3. **错误是可降级的**：`variant error { denied(string) | invalid(string) | failed(string) }`，
   缺能力返回 `denied` 而不是 trap，和 openidea 同样的理由。
4. **可空性是语义**：`stream-summary.request-bytes: option<u64>` —— 流式 body 时大小真的不知道，
   返回 0 是撒谎。

回调顺序（这是规范，不是实现细节）：

```
request_filter.on-request-headers   (每请求一次)
  ├─ 只在 body: buffered|stream 时: request_filter.on-request-body  (0..N)
  └─ 有 trailer 时:                 request_filter.on-request-trailers
        ↓ 上游
response_filter.on-response-headers  (每响应一次)
  ├─ 同上: response_filter.on-response-body (0..N)
  └─ 同上: response_filter.on-response-trailers
response_filter.on-log               (恰好一次，无论成败)
```

**相对 Spire 内建中间件的位置**（固定契约，写进文档以避免"顺序靠猜"）：

```
请求方向:  forward_headers / request_headers  →  所有 WASM 插件（按配置顺序）  →  上游
响应方向:  所有 WASM 插件（按配置顺序）  →  cors / rewrite_headers / compression
```

即：插件看到的是"上游原样响应"，而它加的 header 仍然会被压缩/CORS 处理。

---

## 7. 配置

### 7.1 插件包（`plugin.toml`）

```toml
id = "io.example.header-annotator"
name = "Header Annotator"
version = "0.1.0"
api = "spire:filter@0.1.0"      # 不兼容的组件在加载期就被拒绝
wasm = "dist/plugin.wasm"       # 必须在包目录内（`..`/绝对路径直接拒）

[capabilities]
required = ["host.log", "host.metrics", "host.shared-data", "host.clock"]
optional = []
```

校验规则（`manifest.rs`）：id 形状、semver、`api` 的 major.minor 匹配、`wasm` 不得逃逸包目录
（含 symlink 用 `canonicalize` 复核）、能力必须在白名单内。
**一个坏包不会让网关起不来**：它进 `FAILED` 表，只有引用它的路由会带具体原因报错。

### 7.2 路由（`kind: wasm`）

```yaml
middlewares:
  - kind: wasm
    plugin: io.example.header-annotator
    config: |
      header=x-served-by
      value=edge-a
    fail: open          # open | closed
    body: headers_only  # headers_only | buffered | stream
    timeout_ms: 5
    fuel: 2000000
    max_memory_bytes: 16777216
```

**所有上限都是运维控制的**，manifest 只能声明默认值、不能抬高。`config` 对网关完全不透明
（不想为了一个字段去解析别人的 JSON）。

### 7.3 特征开关与"拒绝而不是忽略"

`Cargo.toml`：

```toml
[features]
default = []
wasm = ["dep:wasmtime", "dep:wasmtime-wasi", "dep:toml"]
```

`WasmPluginConfig` 和 `MiddleWares::Wasm` **无条件编译**，只有执行部分在 feature 后面。
于是没有 `--features wasm` 的构建：

* 仍然能解析 `kind: wasm`；
* `spire validate`、启动、`SIGHUP` 重载都会明确报错：
  "this build has no WASM plugin support (`kind: wasm`); rebuild with `--features wasm`"。

这是刻意的：**静默忽略一个运维配的安全控制**，比启动失败危险得多
（Envoy 用 `MissingConfigFilter` 注入 500 也是同一个思路）。

---

## 8. 与现有代码的集成点（精确位置）

| 位置 | 改动 |
|---|---|
| `middleware/middlewares.rs` | `MiddleWares::Wasm(WasmPluginConfig)`；新增 `RequestPhase` / `ResponsePhase` / `ExchangeSummary`；trait 增加 3 个**带默认实现**的钩子 `on_request_phase` / `on_response_phase` / `on_log_phase` |
| `proxy/http1/http_proxy.rs` | `apply_request_middlewares` 改为接收 `&mut Request<B>`、先跑内建头改写再跑插件；新增 `apply_response_phases`（在 `handle_before_response` 之前）、`apply_log_phases`（之后）；`denial_response()` 抽出复用的本地响应构造 |
| `proxy/http1/http_proxy.rs` | 插件开销门控：`has_plugins` 为假时**不生成** request id、不复制 method/uri |
| `vojo/app_config.rs` | `plugins_dir: Option<String>`（默认 `~/.spire/plugins`） |
| `app/run.rs` | `wasm::init()` + `validate_references()`，在监听器启动前 |
| `configuration_service/app_config_service.rs` | `reload_from_disk`（`SIGHUP` 路径）在**提交前**校验插件引用；先重编译插件目录 |
| `command/validate_command.rs` | `spire validate` 也校验 `kind: wasm`（CI 里拦住坏配置） |
| `monitor/prometheus_exporter.rs` | `spire_wasm_calls_total` / `spire_wasm_call_duration_seconds` / `spire_wasm_traps_total` / `spire_wasm_instances` |

**为什么钩子是"新增+默认实现"而不是改签名**：`check_request` / `handle_response` 的签名
被 10 个内建中间件和 60+ 个测试使用，改签名会把这次改造变成一次大范围重构，
风险与收益不成比例。默认实现为空 ⇒ 内建中间件零改动、零行为变化。

**顺序细节**：插件按它们在 `middlewares` 里出现的顺序执行；但它们作为一个**组**
排在内建中间件之外（请求在 `request_headers` 之后，响应在 `compression`/`cors` 之前）。
这是显式的契约，见 §6。

---

## 9. 可观测性

宿主指标（只读 observability 监听上的 `/metrics`，默认 `127.0.0.1:8870`）：

```
spire_wasm_calls_total{plugin,phase,outcome}          outcome: ok|trap|out_of_fuel|timeout|panic|error
spire_wasm_call_duration_seconds{plugin,phase}        直方图
spire_wasm_traps_total{plugin,reason}
```

> `spire_wasm_instances`（当前存活实例数）**没有**在 M1 落地：线程本地池无法在不引入
> 全局原子的情况下准确上报，而"翻倍计数再减半"的假指标比没有指标更糟。
> M2 会随"全局实例数上限"一起把它做成真的（创建 +1、销毁 -1，每实例一次原子操作，不在热路径）。

插件自定义指标：`spire_plugin_<plugin_sanitized>_<name>_total{plugin="<id>", ...}`。

> 实现时踩到的真实约束：Prometheus 要求**同一个 fq_name 的所有 collector 共享 help 与 label 集合**
> （`prometheus-0.14/src/desc.rs` 的 `dim_hash`）。所以插件 ID 必须进指标名，
> 只放在 const label 里不够——否则两个插件用同一个计数器名会互相顶掉。
> 已在 `state.rs` 注释里写明，并有测试覆盖。

日志：`host.log` → `tracing`，带 `plugin=<id>` 字段，每次交换最多 64 行。

---

## 10. 性能

### 10.1 未启用插件的路由

只有一次 `spire_context.middlewares.as_ref().is_some_and(|m| has_wasm(m))`
（`O(中间件个数)`，通常 0-3 个 `matches!`）。
request id / method / uri 的复制全部在这条分支内，不会执行。

### 10.2 启用插件

单次 `on-request-headers` 的成本构成：

1. header map → `Vec<Header>`：每个 header 一次小 String 分配（典型的 10-20 个 header）；
2. 一次同步 wasm 调用（component model canonical ABI，`Header`/`HeaderOp` 都是 record + list）；
3. 决策应用：`HeaderMap::insert` × ops。

实测口径说明：本仓库没有为插件路径加基准（`benchmarks/` 里是端到端压测）。
M2 计划加 `criterion` 基准，目标 **< 5 µs/请求/插件**，并对 header 数量做敏感性测试。
如果达不到，优化顺序是：(a) 用线程本地 scratch buffer 复用 `Vec<Header>` 的容量；
(b) 在路由配置里声明"只读/只写哪些 header"，只传子集（类似 Envoy 的 `header_to_metadata`）。

### 10.3 容量

```
插件内存上界 ≈ worker_threads × Σ_plugins(每线程实例数 × max_memory_bytes)
```

`max_memory_bytes` 默认 16 MiB 是**上限**不是预留。要控制峰值就调它，
不要指望 wasm 自己省内存。

---

## 11. 安全模型

| 威胁 | 缓解 |
|---|---|
| 恶意/有 bug 的插件死循环 | fuel + epoch 双重上限，trap 后丢弃实例 |
| 内存炸弹 | `StoreLimits` + `trap_on_grow_failure` |
| 越权读写宿主 | 无 fs/env/socket 导入；能力白名单逐调用检查；guest 只拿到数据副本 |
| 伪造/畸形 header 注入上游 | `HeaderName`/`HeaderValue` 强校验，失败走 `fail` 策略 |
| 指标基数爆炸 | 指标名/标签形状校验、每插件最多 64 个指标、标签值截断 128 字节 |
| 共享状态 OOM | 键数/总字节/单值上限，超限返回 `error::invalid` |
| 打包路径逃逸 | manifest 的 `wasm` 必须是包目录内的相对路径，`canonicalize` 复核 |
| 用插件绕过鉴权 | 文档明确：`fail: open` 的插件不能承担安全职责；鉴权必须 `fail: closed` |

**信任边界**：M1 假设插件包由运维放置（本地目录），**没有签名校验**。
M3 加 ed25519 签名 + `spire plugin install` 校验，才适合从不可信来源获取插件。

---

## 12. 路线图

| 阶段 | 内容 | 状态 |
|---|---|---|
| **M1** | headers/trailers ABI、manifest、能力门控、线程本地实例池、双预算、失败策略、指标、热重载、配置校验、`spire validate`、参考插件 + 端到端测试 | ✅ 已完成 |
| **M2** | ① body 阶段落地（`body: buffered` 先做，`stream` 需 `http_body` 包装 + 背压）② `http_call` 宿主能力（外部鉴权），需要 async 桥：guest 拿 token、宿主异步完成后回调 ③ `.cwasm` 编译缓存 ④ 实例总数上限 + `spire_wasm_instances` 真实双向计数 ⑤ criterion 基准 | 待做 |
| **M3** | 签名校验、`spire plugin {new,build,check,install,list}` CLI、guest 端 SDK crate（`spire-plugin-sdk`，把 `wit_bindgen::generate!` + no-op 默认实现 + mock 宿主打包）、Go/TinyGo SDK | 待做 |

M2 里 body 阶段的**关键设计约束**（先写下来，避免以后返工）：
`stream` 模式不能简单在 `poll_frame` 里同步调 wasm——一次 `poll_frame` 可能在一个
CPU-bound 的 future 里，无法承受毫秒级阻塞。可选路径：(a) 只允许 `buffered`
在数据面（简单、有内存上限、牺牲首字节）；(b) `stream` 走 `spawn_blocking` +
有界 channel 的背压。倾向 (a) 先落地，(b) 单独立项。

---

## 13. 已知限制（诚实清单）

1. **body 阶段未接通**：WIT 已定义、宿主不调用。写 body 逻辑的插件现在不会被执行。
2. **gRPC 转码路径**：`proxy()` 的 gRPC 分支目前没有接插件钩子（该分支自己 buffer 了 body）。
3. **`check_request` 准入阶段未接插件**：安全类插件请用 `on-request-headers` + `fail: closed`，
   差别是它在路由匹配**之后**执行（能拿到 route id，但也意味着已消耗一次路由查找）。
4. **WASI 是"链接但拒绝"**，不是"不链接"：Rust guest 的 std 要求它存在。
   别把这句写成"完全无 WASI"。
5. **实例无法跨线程共享**：同一插件在同一进程里有 N 份实例（N=worker 线程数）。
   需要全局单例语义的（比如精确全局限流）必须用 `host.shared-data`，它有 4 MiB 上限。
6. **无签名/来源校验**：只适合运维自管的插件目录。
7. **`plugins_dir` 进程内固定**：改它需要重启（`SIGHUP` 重载只重编译目录内容）。
8. **trap 后实例被丢弃**，下一次请求要重新 instantiate（约 0.1-1ms）。连续 trap 的插件会
   表现为"每请求都慢"。M2 要加**熔断**：连续 N 次 trap 后直接按 fail 策略短路，不再尝试。

---

## 14. 怎么跑起来（可复现）

```powershell
cd rust-proxy
cargo build --release --features wasm          # 网关

cd plugins/header-annotator
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2   # guest 组件
New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target/wasm32-wasip2/release/spire_plugin_header_annotator.wasm dist/plugin.wasm

# 装到插件目录
New-Item -ItemType Directory -Force "$HOME\.spire\plugins\io.example.header-annotator\dist" | Out-Null
Copy-Item plugin.toml "$HOME\.spire\plugins\io.example.header-annotator\"
Copy-Item dist/plugin.wasm "$HOME\.spire\plugins\io.example.header-annotator\dist\"

# 用示例配置启动
cargo run --features wasm -- -c config/examples/wasm_plugin.yaml
curl -i http://127.0.0.1:8086/api/x      # 响应带 x-spire-upstream-status
curl -s http://127.0.0.1:8870/metrics | Select-String spire_plugin_
```

验证工具链：

```powershell
cargo test --features wasm            # 含真实组件的端到端 round trip
cargo test                            # 默认构建，插件校验路径仍然生效
cargo run -- validate -c config/examples/wasm_plugin.yaml
```

`validate` 会真的去扫插件目录、编译组件、检查每一处引用，所以它同时是
"配置是否合法" 和 "插件是否可加载" 的检查——CI 里跑它，比在数据面上发现要早得多。
两种失败给的是不同的话：

```text
# 没装插件
[ERROR] route annotated_route: wasm plugin `io.example.header-annotator` is not loaded
        (loaded: []; directory: C:\Users\you\.spire\plugins)

# 构建没开 feature
[ERROR] route annotated_route uses `kind: wasm`, but this build has no WASM plugin support;
        rebuild with `--features wasm`
```

**开发时的小技巧**：`plugins_dir` 可以指到仓库里的 `plugins/` 目录
（扫描规则是 `<dir>/<任意目录名>/plugin.toml`，按 manifest 的 `id` 索引），
所以本地改完 guest 直接 `cargo build`，再 `cargo run --features wasm -- validate -c <配置>` 就能验证，
不需要往 `~/.spire` 拷文件。

端到端测试（`src/wasm/mod.rs::tests::reference_plugin_round_trip`）做的是真事：
把 `plugins/header-annotator/dist/plugin.wasm` 拷进临时插件目录 → `init()` 编译 →
`request_phase` 断言 header 被加 → `response_phase` 断言状态回写 →
`try_log_phase` → 断言 `host.shared-data` 被写、`spire_plugin_*` 指标出现在 `gather()`。
组件没编译时测试会**跳过并打印原因**，所以没装 wasm target 的机器 `cargo test` 仍然通过。

---

## 15. 如果重来一次，会改的三个决定

1. **会给 `Middleware` trait 一开始就设计"带请求行"的钩子**。现在用"新增默认方法"绕过，
   代价是 `handle_request`（只改 header）和 `on_request_phase`（完整视图）两个相似钩子并存，
   读代码的人需要知道为什么。
2. **会把 `RequestPhase` 的字段改成借用 `&Request`**，而不是拥有 `String`。
   现在为了避开生命周期污染（以及"没插件就不付钱"）选了 owned，代价是每条装配了插件的请求
   多 4 次小分配。用一个泛型 `PhaseInput<'a>` 可以两头都拿到，但会把 `http_proxy.rs`
   的函数签名搞得更吵。
3. **插件指标一开始就让插件 ID 进指标名**。绕了一圈才被 Prometheus 的 `dim_hash` 教会
   （见 §9），这和"先写文档再写代码"是一个教训。
