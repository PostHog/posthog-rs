FROM rust:1-bookworm AS builder
ARG CLIENT_MODE=async
WORKDIR /app
COPY . .
RUN if [ "$CLIENT_MODE" = "async" ]; then \
        cargo build --release --manifest-path compliance/adapter/Cargo.toml; \
    elif [ "$CLIENT_MODE" = "blocking" ]; then \
        cargo build --release --manifest-path compliance/adapter/Cargo.toml --no-default-features; \
    else \
        echo "unsupported CLIENT_MODE: $CLIENT_MODE" >&2; \
        exit 1; \
    fi

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/sdk-adapter /usr/local/bin/
ENV COMPRESSION=gzip
EXPOSE 8080
CMD ["sdk-adapter"]
