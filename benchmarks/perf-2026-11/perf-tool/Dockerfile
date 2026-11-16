# Profiling side-car image: perf + flamegraph-rs + inferno.
#
# Why a separate image: perf must run inside the *same PID namespace* as the
# measured process, and it needs CAP_PERFMON/SYS_ADMIN because WSL2 exposes no
# hardware PMU (only software events such as cpu-clock are usable). Keeping it
# out of the spire image leaves the measured container untouched.
#
# The inferno binaries are kept as a fallback for the classic
# `perf script | inferno-collapse-perf | inferno-flamegraph` pipeline.

FROM rust:1.100 AS builder
ARG http_proxy
ARG https_proxy
ENV http_proxy=${http_proxy}
ENV https_proxy=${https_proxy}
RUN cargo install flamegraph --version 0.6.15 --locked \
    || cargo install flamegraph --locked
RUN cargo install inferno --locked || true
RUN mkdir -p /toolbin \
    && cp /usr/local/cargo/bin/flamegraph /toolbin/ \
    && (cp /usr/local/cargo/bin/inferno-* /toolbin/ 2>/dev/null || true) \
    && ls -l /toolbin

FROM ubuntu:24.04
ARG http_proxy
ARG https_proxy
RUN sed -i "s|http://archive.ubuntu.com/ubuntu|http://mirrors.aliyun.com/ubuntu|g; \
            s|http://security.ubuntu.com/ubuntu|http://mirrors.aliyun.com/ubuntu|g" \
            /etc/apt/sources.list.d/ubuntu.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends linux-tools-6.8.0-149 ca-certificates \
    && ln -sf /usr/lib/linux-tools-6.8.0-149/perf /usr/local/bin/perf \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /toolbin/ /usr/local/bin/

WORKDIR /work
ENTRYPOINT ["/bin/bash"]
