# nginx reference configuration for perf-2026-11.
#
# Mirrors benchmarks/nginx/nginx.conf (nginx:1.23.3, epoll, upstream keepalive)
# with two deliberate changes required for a one-variable comparison:
#   * worker_processes is substituted from `nproc`, which honours the container
#     CPU affinity set by --cpuset-cpus. nginx's own `auto` counts online CPUs,
#     which inside Docker is the host's 20, not the cpuset-restricted count.
#   * worker_rlimit_nofile is raised so 4 workers can hold the client's
#     connections.
worker_processes __WORKERS__;
worker_rlimit_nofile 65535;

events {
    use epoll;
    multi_accept on;
    worker_connections 65536;
}

http {
    access_log off;

    upstream web {
        server backend:8080;

        keepalive 100;
        keepalive_requests 1000;
        keepalive_timeout 60s;
    }

    server {
        listen 80;

        location / {
            proxy_read_timeout 300s;
            proxy_connect_timeout 75s;
            access_log off;
            proxy_http_version 1.1;
            proxy_pass http://web;
            proxy_set_header Connection "";
        }
    }
}
