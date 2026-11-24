FROM rust:1.96-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends build-essential cmake perl
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin ecochain-rtc-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10002 ecochain
COPY --from=build /app/target/release/ecochain-rtc-server /usr/local/bin/
USER 10002
# The server shuts down gracefully on SIGINT only.
STOPSIGNAL SIGINT
ENV ECOCHAIN_RTC_CONFIG=/etc/ecochain-rtc/production.toml
CMD ["ecochain-rtc-server", "serve"]
