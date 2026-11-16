# AGENTS.md — Spire

## 1. 项目定位

Spire 是高性能反向代理 / 网关：单 crate Rust 二进制，源码在 `rust-proxy/`（根目录不是 Cargo workspace，所有 cargo 命令先 `cd rust-proxy`）。技术栈 Tokio + Hyper + axum（只读 observability 监听）+ rustls，`edition = "2024"`，Rust 1.85+，`cargo build --release` 产出 `target/release/spire`。

本仓库只有服务端代码，没有前端。

文档分工：`README.md` 是特性、配置参考与 observability/重载语义的权威来源；`docs/plugin-system.md` 是 WASM 插件设计；`rust-proxy/config/examples/*.yaml` 是可运行示例。写代码前先读对应章节与目标模块头的 `//!` 文档，不要凭记忆猜字段名或语义。

## 2. 结构与数据流

- 启动：`src/main.rs` → `app/run.rs::main_with_error()` → CLI 子命令（`vojo/cli.rs`：默认 serve，另有 `validate` / `config expand` / `examples` / `reload` / `convert`），或 `run_app` 拉起配置服务、只读 observability 监听与各端口代理；**没有 HTTP 管理面**，重载靠 `SIGHUP`。
- 请求链路：监听（`proxy/http1`、`proxy/http2`、`proxy/tcp`）→ 路由匹配（`vojo/router.rs`、`vojo/matcher.rs`）→ 请求阶段中间件（`middleware/`，由 `middlewares.rs` 组装）→ 并发预算（`proxy/concurrency.rs`）→ 上游 → 响应阶段中间件 → 指标（`monitor/`）。
- 目录职责：`vojo/` 放配置与数据结构、`AppError`；`configuration_service/` 管配置加载（`config_loader.rs`，统一入口）、热重载与日志；`observability/` 是只读 `/health` + `/metrics` 监听；`tls/` 管证书加载与 ACME 续期；`health_check/` 管主动/被动探测；`command/` 放 CLI 子命令；`utils/` 放公共小工具；`wasm/` 是插件宿主，只在 `--features wasm` 下编译。

## 3. 质量门禁

在 `rust-proxy/` 下三关全过才算改完：

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- clippy **零 warning**：`-D warnings` 已写进门禁，不接受"只是警告"；修 warning 属于本次改动范围。
- 格式化只覆盖你改过的文件：`cargo fmt` 全仓重排会把无关文件卷进 diff。
- 改动 WASM / 插件时追加：

  ```sh
  cargo test --features wasm
  cargo clippy --all-targets --features wasm -- -D warnings
  ```

  `plugins/header-annotator/dist/plugin.wasm` 随仓库提交，没有 `wasm32-wasip2` 目标也能跑真实组件的端到端 round trip。
- 改动配置 schema 或示例时追加 `cargo run -- validate -c config/examples/<name>.yaml`；`cargo test` 里的 `test_config_examples_are_valid` 会解析 `config/examples` 下每个 yaml，新增示例必须能被解析。
- 测试与被测代码同文件（内联 `#[cfg(test)]`），不新建测试 crate；`mockall` / `serial_test` / `tempfile` 只留在 `[dev-dependencies]`，不得进入 release。
- 依赖相对路径的测试从 `rust-proxy/` 运行。
- 交付前自查 `git diff`：无越界改动、无调试残留、无注释掉的代码。

## 4. 代码规范

- **错误**：用 `vojo/app_error.rs` 的 `AppError`（thiserror）+ `app_error!` 宏，以 `?` 传播；新增 `From` 实现放 `app_error.rs`。请求路径上不允许 panic——连接任务 panic 只死一条连接，是最难发现的一类故障。
- **禁止 `unwrap()` / `expect()`**：用 `?`、`match`、`ok_or_else`、默认值兜底；测试代码例外（`clippy.toml` 已 allow-in-tests）。
- **模块根文件只声明子模块**：不放 `struct` / `enum` / `fn` / `impl` / 常量。`src/wasm/mod.rs` 是唯一例外，其逻辑按体量规则拆到 `wasm/engine.rs`、`state.rs`、`pool.rs`。
- **体量硬上限**：
  - 单文件 ≤1500 行（不计空行与纯注释行）。
  - 单函数 ≤200 行（不计空行与纯注释行）。
  - **超限即拆，没有豁免**：改到超限文件或超限函数时，先把超出部分拆成同目录新文件 / 新函数再提交；不允许以"这个文件本来就大"为由继续往里堆。
  - 拆分作业规则：只做机械移动，不改行为、不改公共 API、不顺手改逻辑；拆出的项按最小可见性（优先私有，跨模块才 `pub(crate)`）；新增文件按同一模块目录归属，并在 `mod.rs` 里显式声明。
  - 拆分与功能改动分两次提交或至少分成两个清晰的 diff 块，方便 review；拆分后必须门禁全过。
- **依赖**：沿用 caret 写法，不主动升级；新增依赖要说明理由、替代方案、为什么现有依赖不够；`optional` 依赖必须挂到对应 feature；测试依赖只进 `[dev-dependencies]`。
- **特性开关 = 拒绝而不是忽略**：`--features wasm` 关闭时，`kind: wasm` 的配置必须在 `validate` / 加载阶段报错（`validate_wasm_middlewares`），不允许静默降级为"忽略该中间件"；新增 feature 同样处理。
- **语言**：代码内注释、日志、错误信息、CLI 帮助一律英文；面向人的设计文档可中文（`docs/plugin-system.md` 即是）。
- 零 `unsafe`；`#[allow(...)]` 必须有同行理由。
- 不新增未接线的死代码；不做与任务无关的重构（拆分是规则要求的例外）。
- 不要把阻塞调用放进 async 任务：请求路径里不出现 `std::thread::sleep`、同步文件 IO、阻塞式锁等待。
- 优先用标准库与现有依赖解决问题，保持单一实现，不混用两套做法。

## 5. 架构不变量

- **配置 schema 成套同步**：`vojo/app_config.rs` 的字段（含 `serde(default)` / `skip_serializing_if` 风格）→ README 配置参考表 → `config/examples/*.yaml` → `docker/spires/app_config.yaml` → `benchmarks/spires/app_config.yaml` → 相关 docs。旧字段要么兼容、要么显式报错，不静默忽略。
- **观测面只读**：`observability/` 只提供 `GET /health` 与 `GET /metrics`，默认绑 `127.0.0.1:8870`；不得新增任何能改写配置的 HTTP 端点。配置唯一来源是文件，日志与错误信息里不得出现 token、密钥、证书私钥。
- **重载语义**：`SIGHUP` → `app_config_service::reload_from_disk` 重读配置文件；解析失败只记日志、保留旧配置；监听器可增删，新端口先全部 bind 成功才提交（两阶段），失败整体回滚；同一端口改 `protocol`/`domains`、改 `observability.listen`、改 `log_level` 都需要重启。新增可热更状态时确认它能在该路径上重建。
- **请求热路径**：不加全局锁、不做 per-request 配置深拷贝；指标 label 只用 `route_id`，禁止用原始请求路径（基数无界）；访问日志保持在 `debug` 级。
- **背压与超时不绕过**：在途 permit 必须覆盖到响应体流完（`PermitBody`）；新的转发路径必须走 `proxy/concurrency.rs` 的两层预算（端点信号量 + 进程级内存保险），不另立一套。
- **客户端 IP 只有一层真相**：`X-Forwarded-For` / `X-Real-IP` 只有在 `trusted_proxies` 命中时才可信（`utils/client_ip.rs`），不另写解析。
- **WASM ABI 同步**：`wit/spire-filter.wit` 与 `plugins/header-annotator/wit/spire-filter.wit` 保持一致（测试 `plugin_wit_matches_host_wit` 守着）；新增 host capability 同时改插件 manifest 的 `[capabilities]`。
- **版本号三处同步**：`rust-proxy/Cargo.toml` 的 `version`、`build_and_deploy.bat` 的 `DOCKER_IMAGE_VERSION`、发布 tag。

## 6. 工作方式

**准则**

1. 以暗猜接口为耻，以认真查阅为荣
2. 以模糊执行为耻，以寻求确认为荣
3. 以盲想业务为耻，以人类确认为荣
4. 以创造接口为耻，以复用现有为荣
5. 以跳过验证为耻，以主动测试为荣
6. 以破坏架构为耻，以遵循规范为荣
7. 以假装理解为耻，以诚实无知为荣
8. 以盲目修改为耻，以谨慎重构为荣

**执行**

- 先查证再提问：能自己读代码、文档、配置确认的事实不占用提问轮次。
- 需要用户决策时，一次性提出所有互不依赖的问题，每条说明要什么信息、为什么需要，能给候选或默认值的直接给。
- 动手前用 1~3 句说明修改思路；存在多种可行方案时列出优缺点让用户选。
- 只改任务范围内的代码，最小必要修改；不主动重构无关代码，不因个人偏好改动既有实现，不删除已有功能除非明确要求。体量拆分是规则要求的唯一例外。
- 保持现有代码风格与项目结构；简单机械的改动可以直接做，涉及多文件或行为变化先给方案。
- 不主动升级依赖；修改构建配置、`.cargo/config.toml`、Dockerfile 前先说明影响。
- 不在未明确要求时执行 `docker push`、`cross build` 或发布脚本（`build_and_deploy.bat` 会构建并推送镜像）。
- 环境：默认 Windows 开发机，`.cargo/config.toml` 给 MSVC 静态链接 CRT，`mimalloc` 只在非 msvc 目标生效；跨 Linux 构建走 `prod.bat` 里的 `cross`。
- 不执行 `git reset --hard`、递归删除目录等危险命令；删文件、清目录前确认目标路径。
- 不在代码、日志、错误信息中输出密钥、token、密码。
- 不自动修改 `README.md` 与 `docs/`，除非明确要求。

**回复**

- 中文回复，先给结论与可执行步骤；简单问题简洁答，复杂问题再展开。
- 改完说明：改了什么、改了哪些文件、测试执行情况、需要注意的问题。
- 不贴完整 diff，除非明确要求。
