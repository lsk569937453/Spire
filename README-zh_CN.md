# Spire-下一代高性能云原生反向代理/负载均衡调度器

Spire 是一个高性能的反向代理/负载均衡调度器。它同时也可以在 K8S 集群中用作入口控制器。

## 为什么我们选择 Spire&&Spire 的优点

### 基准测试

我们针对当前主流的代理/负载均衡器(NGINX, Envoy, and Caddy)做了性能测试。测试结果在[这里](https://github.com/lsk569937453/spire/blob/main/benchmarks-zh_CN.md)。
测试结果表明在相同的机器配置下(4 核 8G),在某些指标上(每秒请求数,平均响应时间),Spire 的数据与 NGINX, 水平接近。
在请求延迟上，Spire 的数据要明显好于 NGINX 和 Envoy。

### 所有的基础功能全都是原生语言开发-速度快

Spire 不止是一个反向代理/负载均衡器，而且是一个 API 网关。作为一个 API 网关，Spire 将会涵盖所有的基础功能(黑白名单/授权/熔断限流/灰度发布
,蓝绿发布/监控/缓存/协议转换)。

与其他的网关相比，Spire 的优点是涵盖 API 网关所有的基础服务，并且性能高。

### Kong

Kong 的免费限流插件[不准确](https://github.com/Kong/kong/issues/5311)。如果想要实现更准确的限流，我们不得不买企业版的 Kong。

### Envoy

Envoy 没有内嵌限流功能。Envoy 提供了限流接口让用户自己实现。目前 github 上使用最多的是这个[项目](https://github.com/envoyproxy/ratelimit)。
第一个缺点是该项目只支持固定窗口限流。固定窗口限流算法的坏处是不支持突发流量。  
第二个缺点是每次请求 Envoy 都会通过使用 grpc 去请求限流集群。相比内嵌的限流算法，这其实额外的增加了一次网络跃点。

## 编译

### 开始编译

请先安装 rust，然后执行下面的命令。

```
cd rust-proxy
cargo build --release
```

你可以在 target/release 目录下找到 Spire.

## 下载发行版

从[这里](https://github.com/lsk569937453/spire/releases)下载发行版.

## 配置文件介绍

### 配置 Spire 作为 http 代理

```
static_config:
  log_level: info
services:
  - listen_port: 8084
    service_config:
      server_type: http
      routes:
        - route_id: test_route
          matcher:
            prefix: /
            prefix_rewrite: /
          router: http://192.168.0.0:9393

```

Spire 将会监听 8084 端口然后转发流量到 http://192.168.0.0:9393。

### 配置 Spire 作为文件服务器

```
static_config:
  log_level: info
services:
  - listen_port: 8084
    service_config:
      server_type: http
      routes:
        - route_id: test_route
          matcher:
            prefix: /
            prefix_rewrite: /
          router:
            type: file
            doc_root: D:\code\github\gitstats\kvrocks
          middlewares:
            - type: rewrite_headers
              expires: 24h
              extensions: [js, css, html, png, jpg, gif]
```

Spire 将会监听 8084 端口。

### 启动:

#### Windows 下启动

```
.\target\release\spire.exe -f .\config\app_config_simple.yaml
```

## <span id="api-gateway">API 网关中的基础功能</span>

![alt tag](https://raw.githubusercontent.com/lsk569937453/image_repo/main/api-gateway.png)

## Spire 已经实现了如下功能

- IP 黑白名单
- 授权(Basic Auth,ApiKey Auth)
- 限流(Token Bucket,Fixed Window)
- 路由
- 负载均衡(论询，随机，基于权重,基于 Header)
- 动态配置(Rest Api)
- 健康检查&异常检测
- 免费 Https 证书
- 控制面板
- 监控(Prometheus)

## 将来会实现的功能

- [ ] 协议转换
- [ ] 缓存
