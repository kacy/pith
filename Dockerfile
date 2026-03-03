FROM alpine:3.21 AS builder

RUN apk add --no-cache xz curl

# install zig 0.15.2
RUN curl -L https://ziglang.org/download/0.15.2/zig-linux-x86_64-0.15.2.tar.xz | \
    tar -xJ -C /usr/local && \
    ln -s /usr/local/zig-linux-x86_64-0.15.2/zig /usr/local/bin/zig

WORKDIR /src
COPY build.zig build.zig.zon ./
COPY src/ src/

RUN zig build -Doptimize=ReleaseSafe

# ---

FROM alpine:3.21

RUN addgroup -g 1000 forge && adduser -D -u 1000 -G forge forge

COPY --from=builder /src/zig-out/bin/forge /usr/local/bin/forge

USER forge
ENTRYPOINT ["forge"]
