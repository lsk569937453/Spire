# 基准测试

本次性能测试主要针对主流的代理/负载均衡。测试方法基于此项目[Proxy-Benchmarks](https://github.com/NickMRamirez/Proxy-Benchmarks).

测试的代理列表如下:

- Caddy
- Envoy
- Nginx
- Spire
- Haproxy
- Traefik

## 测试环境&&测试工具

### 测试什么

本次基准测试主要测试各个项目作为反向代理的性能表现。

### 测试环境

我们将在 docker 容器中做性能测试。第一个优点是任何人只要安装了 docker 都能一键启动性能测试。第二点是在 docker swarm 中限制服务的 cpu 和内存比较方便。我们主要启动三个服务:代理服务，后端服务，测试工具服务。这样三个服务都部署在 docker 容器中，三者相互访问时减少了网络通信的延迟。

### 服务配置

每个服务的配置都是 4 核 8G 的 docker 容器。docker 宿主机是 PC(Cpu 是 13th Gen Intel(R) Core(TM) i5-13600K,内存是 32GB)。

### 测试工具

本次测试工具使用[hey](https://github.com/rakyll/hey)。

### 测试参数

测试指令如下：

```
hey -n 100000 -c 250 -m GET http://proxy:80
```

该指令指使用 250 并发去请求[http://proxy:80]，总共请求 10w 次。我们会在同一台机器上执行该指令多次，只统计数据最好的那一个。

## 测试结果如下

Caddy 的测试结果太差，图表中不再展示。在下一章中有全部的测试数据结果(文本)。
![alt tag](https://raw.githubusercontent.com/lsk569937453/image_repo/main/benchmarks2/rps.png)
![alt tag](https://raw.githubusercontent.com/lsk569937453/image_repo/main/benchmarks2/avt.png)
![alt tag](https://raw.githubusercontent.com/lsk569937453/image_repo/main/benchmarks2/ld.png)

## 使用的指令和测试数据结果

### Haproxy(2.7.3)

```
hey -n 100000 -c 250 -m GET http://haproxy:80/

Summary:
  Total:	1.2244 secs
  Slowest:	0.0890 secs
  Fastest:	0.0001 secs
  Average:	0.0030 secs
  Requests/sec:	81674.2776

  Total data:	13300000 bytes
  Size/request:	133 bytes

Response time histogram:
  0.000 [1]     |
  0.005 [93894] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.010 [2752]  |■
  0.015 [383]   |
  0.020 [170]   |
  0.025 [125]   |
  0.030 [281]   |
  0.035 [182]   |
  0.040 [761]   |
  0.045 [1118]  |
  0.050 [333]   |


Latency distribution:
  10% in 0.0006 secs
  25% in 0.0010 secs
  50% in 0.0015 secs
  75% in 0.0024 secs
  90% in 0.0038 secs
  95% in 0.0056 secs
  99% in 0.0417 secs

Details (average, fastest, slowest):
  DNS+dialup:   0.0000 secs, 0.0001 secs, 0.0496 secs
  DNS-lookup:   0.0000 secs, 0.0000 secs, 0.0124 secs
  req write:    0.0000 secs, 0.0000 secs, 0.0391 secs
  resp wait:    0.0025 secs, 0.0000 secs, 0.0492 secs
  resp read:    0.0003 secs, 0.0000 secs, 0.0419 secs

Status code distribution:
  [200] 100000 responses

```

```
启动内存:78MB
波峰内存:82MB
波谷内存:81MB
```

### Spire

```
hey -n 100000 -c 250 -m GET http://spire:6667

Summary:
  Total:	1.5067 secs
  Slowest:	0.0199 secs
  Fastest:	0.0001 secs
  Average:	0.0037 secs
  Requests/sec:	66370.1064

  Total data:	13800000 bytes
  Size/request:	138 bytes

Response time histogram:
  0.000 [1]     |
  0.005 [97642] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.011 [1220]  |
  0.016 [387]   |
  0.021 [26]    |
  0.026 [472]   |
  0.032 [2]     |
  0.037 [0]     |
  0.042 [0]     |
  0.048 [242]   |
  0.053 [8]     |


Latency distribution:
  10% in 0.0009 secs
  25% in 0.0012 secs
  50% in 0.0016 secs
  75% in 0.0024 secs
  90% in 0.0036 secs
  95% in 0.0044 secs
  99% in 0.0118 secs

Details (average, fastest, slowest):
  DNS+dialup:   0.0000 secs, 0.0001 secs, 0.0528 secs
  DNS-lookup:   0.0000 secs, 0.0000 secs, 0.0486 secs
  req write:    0.0000 secs, 0.0000 secs, 0.0481 secs
  resp wait:    0.0020 secs, 0.0001 secs, 0.0448 secs
  resp read:    0.0001 secs, 0.0000 secs, 0.0448 secs

Status code distribution:
  [200] 100000 responses
```

```
启动内存:4MB
波峰内存:35MB
波谷内存:30MB
```

### Envoy(1.22.8)

```
hey -n 100000 -c 250 -m GET http://envoy:8050

Summary:
  Total:	1.6169 secs
  Slowest:	0.0276 secs
  Fastest:	0.0001 secs
  Average:	0.0040 secs
  Requests/sec:	61847.1944

  Total data:	24700000 bytes
  Size/request:	247 bytes

Response time histogram:
  0.000 [1]     |
  0.006 [91321] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.012 [3336]  |■
  0.017 [230]   |
  0.023 [152]   |
  0.029 [3]     |
  0.034 [17]    |
  0.040 [0]     |
  0.046 [591]   |
  0.051 [3292]  |■
  0.057 [1057]  |


Latency distribution:
  10% in 0.0012 secs
  25% in 0.0018 secs
  50% in 0.0026 secs
  75% in 0.0038 secs
  90% in 0.0055 secs
  95% in 0.0191 secs
  99% in 0.0515 secs

Details (average, fastest, slowest):
  DNS+dialup:   0.0000 secs, 0.0001 secs, 0.0571 secs
  DNS-lookup:   0.0000 secs, 0.0000 secs, 0.0096 secs
  req write:    0.0000 secs, 0.0000 secs, 0.0535 secs
  resp wait:    0.0049 secs, 0.0001 secs, 0.0561 secs
  resp read:    0.0002 secs, 0.0000 secs, 0.0431 secs

Status code distribution:
  [200] 100000 responses
```

```
启动内存:17MB
波峰内存:36MB
波谷内存:33MB
```

### Traefik(2.9.8)

```
hey -n 100000 -c 250 -m GET http://traefik:80/

Summary:
  Total:	1.6810 secs
  Slowest:	0.0256 secs
  Fastest:	0.0001 secs
  Average:	0.0041 secs
  Requests/sec:	59486.9083

  Total data:	28800000 bytes
  Size/request:	288 bytes

Response time histogram:
  0.000 [1]	|
  0.003 [29996]	|■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.005 [43114]	|■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.008 [18210]	|■■■■■■■■■■■■■■■■■
  0.010 [6093]	|■■■■■■
  0.013 [1868]	|■■
  0.015 [487]	|
  0.018 [144]	|
  0.021 [36]	|
  0.023 [36]	|
  0.026 [15]	|


Latency distribution:
  10% in 0.0013 secs
  25% in 0.0024 secs
  50% in 0.0036 secs
  75% in 0.0054 secs
  90% in 0.0074 secs
  95% in 0.0090 secs
  99% in 0.0122 secs

Details (average, fastest, slowest):
  DNS+dialup:	0.0000 secs, 0.0001 secs, 0.0256 secs
  DNS-lookup:	0.0000 secs, 0.0000 secs, 0.0142 secs
  req write:	0.0000 secs, 0.0000 secs, 0.0089 secs
  resp wait:	0.0039 secs, 0.0001 secs, 0.0229 secs
  resp read:	0.0002 secs, 0.0000 secs, 0.0082 secs

Status code distribution:
  [200]	100000 responses

```

```
启动内存:22MB
波峰内存:135MB
波谷内存:120MB
```

### Nginx(1.23.3)

```
 hey -n 100000 -c 250 -m GET http://nginx:80/

Summary:
  Total:        0.6726 secs
  Slowest:      0.0644 secs
  Fastest:      0.0001 secs
  Average:      0.0016 secs
  Requests/sec: 148676.5570

  Total data:   14600000 bytes
  Size/request: 146 bytes

Response time histogram:
  0.000 [1]     |
  0.006 [99550] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.013 [199]   |
  0.019 [0]     |
  0.026 [0]     |
  0.032 [0]     |
  0.039 [0]     |
  0.045 [0]     |
  0.052 [127]   |
  0.058 [75]    |
  0.064 [48]    |


Latency distribution:
  10% in 0.0006 secs
  25% in 0.0008 secs
  50% in 0.0013 secs
  75% in 0.0019 secs
  90% in 0.0029 secs
  95% in 0.0035 secs
  99% in 0.0049 secs

Details (average, fastest, slowest):
  DNS+dialup:   0.0000 secs, 0.0001 secs, 0.0644 secs
  DNS-lookup:   0.0000 secs, 0.0000 secs, 0.0534 secs
  req write:    0.0000 secs, 0.0000 secs, 0.0581 secs
  resp wait:    0.0014 secs, 0.0001 secs, 0.0571 secs
  resp read:    0.0002 secs, 0.0000 secs, 0.0509 secs

Status code distribution:
  [200] 100000 responses
```

```
启动内存:29MB
波峰内存:37MB
波谷内存:34MB
```

### Caddy(2.6.4)

```
hey -n 100000 -c 250 -m GET http://caddy:80/

Summary:
  Total:	5.6219 secs
  Slowest:	0.1762 secs
  Fastest:	0.0001 secs
  Average:	0.0137 secs
  Requests/sec:	17787.6741

  Total data:	20900000 bytes
  Size/request:	209 bytes

Response time histogram:
  0.000 [1]     |
  0.029 [82204] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.059 [1879]  |■
  0.088 [11399] |■■■■■■
  0.117 [3856]  |■■
  0.146 [191]   |
  0.175 [37]    |
  0.205 [314]   |
  0.234 [105]   |
  0.263 [7]     |
  0.292 [7]     |


Latency distribution:
  10% in 0.0013 secs
  25% in 0.0026 secs
  50% in 0.0049 secs
  75% in 0.0099 secs
  90% in 0.0761 secs
  95% in 0.0867 secs
  99% in 0.1065 secs

Details (average, fastest, slowest):
  DNS+dialup:   0.0000 secs, 0.0001 secs, 0.2924 secs
  DNS-lookup:   0.0000 secs, 0.0000 secs, 0.0064 secs
  req write:    0.0000 secs, 0.0000 secs, 0.0102 secs
  resp wait:    0.0181 secs, 0.0001 secs, 0.2924 secs
  resp read:    0.0001 secs, 0.0000 secs, 0.0102 secs

Status code distribution:
  [200] 100000 responses

```

```
启动内存:10MB
波峰内存:60MB
波谷内存:41MB
```

## 我想自己复现一下测试怎么办

所有的测试都在[测试目录](https://github.com/lsk569937453/spire/tree/main/benchmarks)下。以 Nginx 为例，可以直接进入测试目录下的[Nginx 目录](https://github.com/lsk569937453/spire/tree/main/benchmarks/nginx)。修改 Nginx 文件后，然后使用如下的命令启动测试集群

```
docker stack deploy --compose-file docker-compose.yaml benchmark
```

等测试集群启动完成，进入如下指令进入 test 容器。

```
docker exec -it xxxx /bin/bash
```

然后使用如下指令启动测试

```
hey -n 100000 -c 250 -m GET http://nginx:80/
```

## 后记

我一共做了两次大的测试，这次的结果和上次不一样。
上次的测试环境是 windows 10 下安装 docker 容器，然后启动测试。这次的测试环境是 ubuntu 22.04 安装 docker 容器，然后启动测试。  
两次测试结果的不同点是 nginx 的性能有所下降，envoy 和 silverWind 的性能有略微上升。猜测可能是 ubuntu 和 windows 的底层使用的虚拟化技术不一样导致的。
