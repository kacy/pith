FROM alpine:3.21.6@sha256:c3f8e73fdb79deaebaa2037150150191b9dcbfba68b4a46d70103204c53f4709 AS builder

RUN apk add --no-cache xz curl

# install zig 0.15.2
RUN curl -fsSL -o /tmp/zig.tar.xz https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz && \
    echo "02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239  /tmp/zig.tar.xz" | sha256sum -c - && \
    tar -xJf /tmp/zig.tar.xz -C /usr/local && \
    rm /tmp/zig.tar.xz && \
    ln -s /usr/local/zig-linux-x86_64-0.15.2/zig /usr/local/bin/zig

WORKDIR /src
COPY build.zig build.zig.zon ./
COPY src/ src/

RUN zig build -Doptimize=ReleaseSafe

# ---

FROM alpine:3.21.6@sha256:c3f8e73fdb79deaebaa2037150150191b9dcbfba68b4a46d70103204c53f4709

RUN addgroup -g 1000 pith && adduser -D -u 1000 -G pith pith

COPY --from=builder /src/zig-out/bin/pith /usr/local/bin/pith

USER pith
ENTRYPOINT ["pith"]
