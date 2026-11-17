FROM node:26-trixie-slim AS ui
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1.100-slim-trixie AS server
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY --from=ui /web/build web/build
RUN cargo build --release --locked -p violet

FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=server /app/target/release/violet /usr/local/bin/violet
ENV VIOLET_LISTEN=0.0.0.0:8080 VIOLET_LIBRARY=/library
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/violet", "serve"]
