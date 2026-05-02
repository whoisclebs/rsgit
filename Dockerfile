FROM rust:1.86-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --gid nogroup --home-dir /nonexistent --shell /usr/sbin/nologin rsgit
COPY --from=build /src/target/release/rsgit /usr/local/bin/rsgit
USER 10001:65534
ENV RSGIT_ADDR=0.0.0.0:8080 \
    RSGIT_REPO_ROOT=/repos \
    RSGIT_GIT=/usr/bin/git
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/rsgit"]
