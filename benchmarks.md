# Benchmarks

Tests performance of various proxies/load balancers. Based on the [Proxy-Benchmarks](https://github.com/NickMRamirez/Proxy-Benchmarks).

We test the following proxies:

- Caddy
- Envoy
- NGINX
- Spire

## Setup

We use the **docker-compose** to do the performance test.Install the docker on your computer and confirm that the your computer have enough cpu and memory.There are three services in the docker-compose including the hey(Testing Tool),proxy and the backend.We limit **the cpu cores(4 core) and memory(8GB)** for the service.

Our testing environment is based on the PC.And the cpu of the PC is 13th Gen Intel(R) Core(TM) i5-13600K,the memory of the PC is 32GB.

## Results using Hey

![alt tag](https://raw.githubusercontent.com/lsk569937453/image_repo/main/benchmarks2/rps.png)
![alt tag](https://raw.githubusercontent.com/lsk569937453/image_repo/main/benchmarks2/avt.png)
![alt tag](https://raw.githubusercontent.com/lsk569937453/image_repo/main/benchmarks2/ld.png)

Graphs created using [https://www.rapidtables.com/tools/bar-graph.html](https://www.rapidtables.com/tools/bar-graph.html)

### Haproxy(2.7.3)

```
hey -n 100000 -c 250 -m GET http://haproxy:80/

Summary:
  Total:        1.1709 secs
  Slowest:      0.0496 secs
  Fastest:      0.0001 secs
  Average:      0.0029 secs
  Requests/sec: 85406.0342

  Total data:   15300000 bytes
  Size/request: 153 bytes

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

### Spire

```
hey -n 100000 -c 250 -m GET http://spire:6667
Summary:
  Total:        0.9278 secs
  Slowest:      0.0662 secs
  Fastest:      0.0001 secs
  Average:      0.0023 secs
  Requests/sec: 107776.3415

  Total data:   15500000 bytes
  Size/request: 155 bytes

Response time histogram:
  0.000 [1]     |
  0.007 [98550] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.013 [729]   |
  0.020 [220]   |
  0.027 [250]   |
  0.033 [0]     |
  0.040 [0]     |
  0.046 [0]     |
  0.053 [66]    |
  0.060 [113]   |
  0.066 [71]    |


Latency distribution:
  10% in 0.0009 secs
  25% in 0.0012 secs
  50% in 0.0017 secs
  75% in 0.0026 secs
  90% in 0.0038 secs
  95% in 0.0046 secs
  99% in 0.0092 secs

Details (average, fastest, slowest):
  DNS+dialup:   0.0000 secs, 0.0001 secs, 0.0662 secs
  DNS-lookup:   0.0001 secs, 0.0000 secs, 0.0621 secs
  req write:    0.0000 secs, 0.0000 secs, 0.0616 secs
  resp wait:    0.0020 secs, 0.0001 secs, 0.0557 secs
  resp read:    0.0002 secs, 0.0000 secs, 0.0524 secs

Status code distribution:
  [200] 100000 responses
```

### Envoy(1.22.8)

```
hey -n 100000 -c 250 -m GET http://envoy:8050

Summary:
  Total:        2.0816 secs
  Slowest:      0.0571 secs
  Fastest:      0.0001 secs
  Average:      0.0051 secs
  Requests/sec: 48040.4346

  Total data:   29400000 bytes
  Size/request: 294 bytes

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

### Caddy(2.6.4)

```
hey -n 100000 -c 250 -m GET http://caddy:80/

Summary:
  Total:        7.5941 secs
  Slowest:      0.2924 secs
  Fastest:      0.0001 secs
  Average:      0.0183 secs
  Requests/sec: 13168.1413

  Total data:   25700000 bytes
  Size/request: 257 bytes

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
