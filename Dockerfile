# ==========================================
# Etapa 1: Builder
# ==========================================
FROM rust:slim-bookworm AS builder

WORKDIR /app

# Instala dependências de compilação
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

# Cache de dependências: compila com um main.rs vazio antes de copiar o código real
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs     && cargo build --release     && rm -rf src target/release/rustyfin target/release/deps/rustyfin*

# Copia o código-fonte e compila o binário final
COPY src ./src
# `touch`: os arquivos copiados podem ter data anterior ao build das dependências e o Cargo
# reaproveitaria o binário vazio do passo de cache.
RUN find src -name '*.rs' -exec touch {} + && cargo build --release

# ==========================================
# Etapa 2: Runtime Mínimo
# ==========================================
FROM debian:bookworm-slim AS runtime

# Instala ffmpeg/ffprobe para extração de metadados e codecs
RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg \
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copia o executável compilado da etapa de build
COPY --from=builder /app/target/release/rustyfin /app/rustyfin

# Cria diretórios de volume padrão
RUN mkdir -p /media /data

# Variáveis de ambiente padrão para container
ENV RUSTYFIN_HOST=0.0.0.0 \
    RUSTYFIN_PORT=8096 \
    DATABASE_URL="sqlite:///data/rustyfin.db?mode=rwc" \
    RUSTYFIN_MEDIA_DIRS="/media" \
    RUSTYFIN_CACHE_DIR="/data/cache" \
    RUST_LOG="rustyfin=info,tower_http=info"

EXPOSE 8096

VOLUME ["/media", "/data"]

ENTRYPOINT ["/app/rustyfin"]
