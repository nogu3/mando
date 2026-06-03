# 単一バイナリ（index.html 焼き込み済み）をビルドして配る。
# config は焼き込まず外から渡す（実 IP・EPC は設置環境ごとのデプロイデータ）。
FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
COPY index.html ./index.html
RUN cargo build --release

FROM debian:stable-slim
# enl / casa を同梱する場合はここに COPY する（バックエンドのバイナリ）。
# 例: COPY --from=enl-build /usr/local/bin/enl /usr/local/bin/enl
COPY --from=build /src/target/release/mando /usr/local/bin/mando
ENV MANDO_CONFIG=/config/config.toml
EXPOSE 8080
ENTRYPOINT ["mando"]
