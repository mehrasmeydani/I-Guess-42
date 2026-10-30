# ---- build ----------------------------------------------------------------
# sqlx compiles SQLite from source (libsqlite3-sys/bundled), so the builder
# needs a C toolchain. Askama templates and the migrations are both embedded
# into the binary at compile time; `static/` is served at runtime and also
# fingerprinted at compile time.
#
# Both base images are pinned by digest. A bare tag is re-resolved on every
# build, and the official images are republished whenever Debian ships a fix;
# the moment one moves, every layer under it misses the cache, and a release
# that only touched src/ recompiles all 281 crates. Dependabot proposes new
# digests on `dev`, so that slow build happens when someone merges it.
FROM rust:1-slim-bookworm@sha256:86234e857a413004348b471d11a4ebe1aa92d2eae2b6b143cac68fd1ac3c48be AS builder

RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential pkg-config \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Warm the dependency layer first so source edits don't trigger a full rebuild.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
 && cargo build --release \
 && rm -rf src

COPY src ./src
COPY templates ./templates
COPY migrations ./migrations
# What the footer prints. Its own file, and copied here rather than up with
# Cargo.toml, so that bumping it for a release does not invalidate the
# dependency layer above and recompile all 281 crates.
COPY version.txt ./
# The stylesheet and script are hashed into the binary (templates.rs,
# ASSET_VERSION) for cache-busting, so the build needs them too.
COPY static ./static
# Cargo skips a rebuild if mtime looks unchanged after the dummy main.rs above.
RUN touch src/main.rs && cargo build --release

# ---- runtime --------------------------------------------------------------
FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8

# ca-certificates is required to reach https://api.intra.42.fr.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home --home-dir /app app

WORKDIR /app
COPY --from=builder /build/target/release/i_guess_42 /usr/local/bin/i_guess_42
COPY static ./static

# The SQLite file lives here; mount a volume so it outlives the container.
RUN mkdir -p /app/data && chown -R app:app /app
VOLUME ["/app/data"]
USER app

ENV BIND_ADDR=0.0.0.0:3000 \
    DATABASE_URL=sqlite:///app/data/game.db \
    RUST_LOG=i_guess_42=info
EXPOSE 3000

CMD ["i_guess_42"]
