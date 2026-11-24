FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends gcc make && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

RUN cargo build --release --locked && \
    ./target/release/pith build self-host/ir_driver.pith && \
    ./target/release/pith build self-host/pith_main.pith

# ---

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates gcc libc6 && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd -g 1000 pith && \
    useradd -m -u 1000 -g pith pith

COPY --from=builder /src/target/release/pith /usr/local/bin/pith
COPY --from=builder /src/target/release/libpith_runtime.a /usr/local/lib/pith/libpith_runtime.a
COPY --from=builder /src/self-host/pith_main /opt/pith/self-host/pith_main
COPY --from=builder /src/self-host/ir_driver /opt/pith/self-host/ir_driver
COPY --from=builder /src/std /opt/pith/std

ENV PITH_HOME=/opt/pith
ENV PITH_RUNTIME_LIB=/usr/local/lib/pith/libpith_runtime.a
ENV PITH_SELF_HOST=/opt/pith/self-host/pith_main
ENV PITH_IR_DRIVER=/opt/pith/self-host/ir_driver

USER pith
ENTRYPOINT ["pith"]
