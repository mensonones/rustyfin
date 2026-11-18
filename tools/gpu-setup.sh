#!/bin/sh
# Prepara o docker compose para o Rustyfin usar a GPU Intel ou AMD do host (Linux).
#
# Detecta o dispositivo de renderização (/dev/dri/renderD*), descobre o grupo dono dele e grava no `.env`:
#   RENDER_GID=<gid do grupo>
#   COMPOSE_FILE=docker-compose.yml:docker-compose.gpu.yml
# Depois disso, `docker compose up -d` já repassa a GPU ao container. É seguro rodar mais de uma vez.
#
# Variáveis opcionais: DRI_DIR (padrão /dev/dri) e ENV_FILE (padrão .env).
set -eu

DRI_DIR="${DRI_DIR:-/dev/dri}"
ENV_FILE="${ENV_FILE:-.env}"

node=""
for f in "$DRI_DIR"/renderD*; do
  if [ -e "$f" ]; then
    node="$f"
    break
  fi
done

if [ -z "$node" ]; then
  echo "Nenhuma GPU encontrada em $DRI_DIR (não há renderD*)." >&2
  echo "O Rustyfin continuará transcodificando no processador (libx264)." >&2
  echo "Dica: no Docker Desktop (Windows/macOS) a GPU Intel não é repassada ao container." >&2
  exit 1
fi

gid=$(stat -c %g "$node")
echo "GPU: $node (grupo $gid)"

touch "$ENV_FILE"
# remove a linha antiga do RENDER_GID e grava a nova
grep -v '^RENDER_GID=' "$ENV_FILE" > "$ENV_FILE.tmp" || true
echo "RENDER_GID=$gid" >> "$ENV_FILE.tmp"
mv "$ENV_FILE.tmp" "$ENV_FILE"

# COMPOSE_FILE: cria, ou acrescenta o arquivo da GPU se já existir outro valor
if grep -q '^COMPOSE_FILE=' "$ENV_FILE"; then
  if ! grep '^COMPOSE_FILE=' "$ENV_FILE" | grep -q 'docker-compose.gpu.yml'; then
    sed 's|^COMPOSE_FILE=\(.*\)$|COMPOSE_FILE=\1:docker-compose.gpu.yml|' "$ENV_FILE" > "$ENV_FILE.tmp"
    mv "$ENV_FILE.tmp" "$ENV_FILE"
  fi
else
  echo "COMPOSE_FILE=docker-compose.yml:docker-compose.gpu.yml" >> "$ENV_FILE"
fi

echo "Gravado em $ENV_FILE: RENDER_GID=$gid e COMPOSE_FILE com docker-compose.gpu.yml."
echo
echo "Agora recrie o container:   docker compose up -d"
echo "E confira no log:           docker compose logs rustyfin | grep -i 'aceleração'"
echo "Diagnóstico da GPU:         docker compose exec rustyfin vainfo --display drm --device /dev/dri/renderD128"
