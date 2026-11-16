# Downstream used by the perf-2026-11 benchmark.
#
# The class files are compiled on the host (JDK 21) because the benchmark only
# needs a JRE at runtime; compiling inside the image would pull a full JDK into
# the measured topology.
FROM eclipse-temurin:8-jdk

RUN mkdir -p /app/classes /chaos

WORKDIR /app

COPY classes/ /app/classes/

EXPOSE 8080

CMD ["java", "-cp", "/app/classes", "Backend"]
