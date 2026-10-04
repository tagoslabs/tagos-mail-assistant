# rmcp 3.5 needs rustc 1.88 and the locked uuid crate needs 1.89.
FROM rust:1.89-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p tagos-mail-assistant-demo

FROM debian:bookworm-slim
RUN useradd --create-home --uid 10001 --shell /usr/sbin/nologin tagos
COPY --from=build /src/target/release/tagos-mail-assistant-demo /usr/local/bin/tagos-mail-assistant-demo
WORKDIR /home/tagos
USER tagos
EXPOSE 8787
ENTRYPOINT ["tagos-mail-assistant-demo"]
CMD ["serve", "--port", "8787", "--listen", "0.0.0.0"]
