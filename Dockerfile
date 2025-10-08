ARG APPNAME=tg-auction-bot

ARG OUTDIR=target/release/

FROM rust:1.90-bookworm AS chef

# Install build tools
RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
RUN cargo binstall cargo-chef -y
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Cook the dependencies using the recipe prepared earlier
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN SQLX_OFFLINE=true cargo build --release
FROM debian:bookworm-slim AS runtime

ARG OUTDIR
ARG APPNAME

WORKDIR /usr/local/bin
RUN apt-get update \
  && apt-get install -y libssl-dev pkg-config ca-certificates curl \
  && apt-get clean && update-ca-certificates
COPY --from=builder /app/$OUTDIR /usr/local/bin

ENTRYPOINT ["/usr/local/bin/tg-auction-bot"]
