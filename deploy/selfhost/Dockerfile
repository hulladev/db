FROM rust:1.98.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build -p hulladb --example selfhost --release --locked

FROM debian:bookworm-slim
RUN groupadd --gid 10001 hulladb && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin hulladb
COPY --from=build /build/target/release/examples/selfhost /usr/local/bin/hulladb-selfhost
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=4s --start-period=20s --retries=3 CMD ["/usr/local/bin/hulladb-selfhost", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/hulladb-selfhost"]
CMD ["serve"]
