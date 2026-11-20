# syntax=docker/dockerfile:1
FROM rust:1.98-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target cargo build --locked --release -p metiva && cp target/release/metiva /usr/local/bin/metiva

FROM debian:bookworm-slim AS service
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/metiva /usr/local/bin/metiva
ENV METIVA_DATA=/data
USER 1000:1000
ENTRYPOINT ["metiva"]
CMD ["api"]

FROM node:22-bookworm-slim AS web
WORKDIR /src/apps/web
COPY apps/web/package*.json ./
RUN npm ci --no-audit --no-fund
COPY apps/web ./
COPY assets /src/assets
RUN npm run build

FROM nginx:1.28-alpine AS ingress
COPY nginx/default.conf /etc/nginx/conf.d/default.conf
COPY --from=web /src/apps/web/dist /usr/share/nginx/html
