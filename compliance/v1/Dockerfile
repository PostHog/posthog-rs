FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p sdk-adapter

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/sdk-adapter /usr/local/bin/
ENV CAPTURE_MODE=v1
EXPOSE 8080
CMD ["sdk-adapter"]
