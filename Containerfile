FROM rust:1.96-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN groupadd --gid 10001 slopbox && useradd --uid 10001 --gid 10001 --no-create-home slopbox \
    && mkdir /data && chown slopbox:slopbox /data
COPY --from=build /src/target/release/slopbox /usr/local/bin/slopbox
USER 10001:10001
VOLUME /data
EXPOSE 3000
ENV STORAGE_PATH=/data BIND_ADDR=0.0.0.0:3000
ENTRYPOINT ["/usr/local/bin/slopbox"]
CMD ["serve"]
