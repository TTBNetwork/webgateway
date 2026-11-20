# syntax=docker/dockerfile:1

FROM node:24-bookworm-slim AS frontend
WORKDIR /src
RUN npm install -g pnpm@12.14.0
COPY dashboard/frontend/package.json dashboard/frontend/pnpm-lock.yaml dashboard/frontend/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY dashboard/frontend/ ./
RUN pnpm run build

FROM rust:1.100-bookworm AS builder
WORKDIR /src
ENV CARGO_PROFILE_RELEASE_STRIP=symbols
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY dashboard/backend ./dashboard/backend
COPY gateway ./gateway
COPY mnt ./mnt
COPY vendor ./vendor
RUN cargo build --release --locked -p dashboard -p gateway -p webgateway-mnt

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl libgcc-s1 \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 webgateway \
 && useradd --system --uid 10001 --gid webgateway --home-dir /app --shell /usr/sbin/nologin webgateway

WORKDIR /app
COPY --from=builder /src/target/release/dashboard /app/bin/dashboard
COPY --from=builder /src/target/release/gateway /app/bin/gateway
COPY --from=builder /src/target/release/webgateway-mnt /app/bin/webgateway-mnt
COPY --from=frontend /src/dist /app/panel
COPY docker/entrypoint.sh /usr/local/bin/webgateway-entrypoint
RUN chmod 755 /app/bin/dashboard /app/bin/gateway /app/bin/webgateway-mnt /usr/local/bin/webgateway-entrypoint \
 && mkdir -p /run/webgateway \
 && chown -R webgateway:webgateway /app /run/webgateway

ENV DASHBOARD_STATIC_DIR=/app/panel \
    MNT_SOCKET=/run/webgateway/mnt.sock
USER webgateway
EXPOSE 3000 80 443 8080
ENTRYPOINT ["/usr/local/bin/webgateway-entrypoint"]
CMD ["dashboard"]
