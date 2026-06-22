FROM rust:stable-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
COPY dashboard.html ./

RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot

ENV SQUEEZE_HOST=0.0.0.0

COPY --from=builder /app/target/release/toksqz /usr/local/bin/toksqz

EXPOSE 8787

ENTRYPOINT ["/usr/local/bin/toksqz"]
