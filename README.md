# Spire-The Next Generation High-Performance Proxy

[![build](https://github.com/printfn/fend/workflows/build/badge.svg)](https://github.com/lsk569937453/spire/actions/workflows/build.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

English [简体中文](./README-zh_CN.md)

The Spire is a high-performance reverse proxy/load balancer. And it could be also used as the ingress
controller in the k8s.

## Why we chose Spire

### Benchmarks

We do the performance testing between several popular proxies including NGINX, Envoy, and Caddy. The benchmarks show [here](https://github.com/lsk569937453/spire/blob/main/benchmarks.md).

The test results show that under the same machine configuration (4 cores 8G), in some indicators (requests per second, average response time), the data of Spire is almost the same as the NGINX and Envoy.
In terms of request latency, Spire is better than NGINX and Envoy.

### All basic functions are developed in native language - fast

Spire is not only a reverse proxy/load balancer, but also an API gateway. As an API gateway, Spire will cover all basic functions (black and white list/authorization/fuse limit/gray release
, blue-green publishing/monitoring/caching/protocol conversion).

Compared with other gateways, Spire has the advantage of covering all the basic services of the API gateway, and has high performance. Second, Spire's dynamic configuration is close to real-time. Every time the configuration is modified, it will take effect within 5 seconds (close to real-time).

### Kong

The free Ratelimiting plugin for Kong is [inaccurate](https://github.com/Kong/kong/issues/5311). If we want to achieve more accurate Ratelimiting, we have to buy the enterprise version of Kong.

### Envoy

Envoy does not have built-in ratelimiting. Envoy provides a ratelimiting interface for users to implement by themselves. Currently the most used is this [project](https://github.com/envoyproxy/ratelimit).
The first disadvantage is that the project only supports fixed-window ratelimiting. The disadvantage of the fixed window ratelimiting is that it does not support burst traffic.
The second disadvantage is that every time Envoy is requested, it will use grpc to request the ratelimiting cluster. Compared with the built-in current limiting algorithm, this actually adds an additional network hop.

## Dynamic Configuration

You could change the configuration over the rest API. And the new configuration will have an effect **in 5 seconds**.

## Compile or Download the release

### Compile

You have to install the rust first.

```
cd rust-proxy
cargo build --release
```

You could get the release in the target/release.

### Download the release

Download the release from the [website](https://github.com/lsk569937453/spire/releases).

## Config Introduction

### Spire as the http proxy

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

The proxy will listen the 9969 port and forward the traffic to the http://localhost:8888/,http://localhost:9999/.http://localhost:7777/.

### Spire as the static file server

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

### Setup:

#### Windows Startup

```
.\target\release\spire.exe -f .\config\app_config_simple.yaml
```

## <span id="api-gateway">The Base Function in Api Gateway</span>

![alt tag](https://raw.githubusercontent.com/lsk569937453/image_repo/main/api-gateway.png)

## Spire has implemented the following functions:

- IP Allow-and-Deny list
- Authentication(Basic Auth,ApiKey Auth)
- Rate limiting(Token Bucket,Fixed Window)
- Routing
- Load Balancing(Poll,Random,Weight,Header Based)
- HealthCheck&AnomalyDetection
- Free Https Certificate
- Dynamic Configuration(Rest Api)
- Dashboard For Spire
- Monitoring(Prometheus)

## Future

- [ ] Protocol Translation
- [ ] Caching
