# Minimal runtime image for Spire.
#
# The binary is built outside this image (see .github/workflows/build.yaml) and
# copied in as ./spire.
FROM library/ubuntu:24.04

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tzdata \
    && rm -rf /var/lib/apt/lists/*

ENV TZ=Asia/Shanghai

# mimalloc is the global allocator on non-MSVC builds. On overcommit systems it
# defaults to eagerly committing whole arenas, which inflates RSS after a burst
# and keeps it there; committing lazily keeps the peak and the post-burst
# footprint close to what is actually live. Override at runtime if you prefer
# the eager-commit trade-off (lower page-fault cost, higher RSS).
ENV MIMALLOC_ARENA_EAGER_COMMIT=0


# Run as an unprivileged user: a gateway only needs to read its config and write
# certificates under its own home directory.
RUN useradd --system --create-home --home-dir /home/spire --shell /usr/sbin/nologin spire

COPY spire /usr/local/bin/spire
COPY app_config.yaml /etc/spire/config.yaml

RUN chmod 0755 /usr/local/bin/spire \
    && chown -R spire:spire /etc/spire \
    && mkdir -p /home/spire/.spire \
    && chown -R spire:spire /home/spire

USER spire
WORKDIR /home/spire

# The observability listener binds to loopback by default, so the probe runs
# inside the container. `/health` needs no authentication.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8870/health || exit 1

EXPOSE 6667
ENTRYPOINT ["/usr/local/bin/spire", "-f", "/etc/spire/config.yaml"]
