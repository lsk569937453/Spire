FROM ubuntu:jammy-20230301
#RUN apt-get update &&\
#    apt-get install -y heaptrack &&\
#    apt-get install -y heaptrack-gui
#RUN apt-get update &&\
#    apt-get install -y valgrind
COPY spire /tmp/spire
COPY app_config.yaml /tmp/app_config.yaml
RUN chmod go+r /tmp/spire
WORKDIR "/tmp"
ENTRYPOINT ["/tmp/spire","-f","/tmp/app_config.yaml"]
#ENTRYPOINT ["tail", "-f", "/dev/null"]

