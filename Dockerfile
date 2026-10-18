FROM rust:1.85-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -r -u 10001 app && mkdir /data && chown app:app /data
COPY --from=build /build/target/release/lark-docs-bot-rs /usr/local/bin/lark-docs-bot-rs
USER app
ENV BIND_ADDR=0.0.0.0:3000 DATA_FILE=/data/documents.json
EXPOSE 3000
VOLUME ["/data"]
CMD ["lark-docs-bot-rs"]
