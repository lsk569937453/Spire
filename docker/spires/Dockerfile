FROM library/ubuntu:25.10
RUN apt-get update &&\
   apt-get install -y curl
#    apt-get install -y heaptrack-gui
#RUN apt-get update &&\
#    apt-get install -y valgrind
# 设置时区和字符集
ENV TZ=Asia/Shanghai
ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    tzdata \
    locales && \
    # 配置时区（两种方式任选其一）
    ln -fs /usr/share/zoneinfo/Asia/Shanghai /etc/localtime && \
    dpkg-reconfigure -f noninteractive tzdata && \
    # 配置字符集
    sed -i '/en_US.UTF-8/s/^# //g' /etc/locale.gen && \
    locale-gen en_US.UTF-8 && \
    # 清理缓存
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*
COPY spire /tmp/spire
COPY app_config.yaml /tmp/app_config.yaml
RUN chmod go+r /tmp/spire
WORKDIR "/tmp"
ENTRYPOINT ["/tmp/spire","-f","/tmp/app_config.yaml"]
#ENTRYPOINT ["tail", "-f", "/dev/null"]

