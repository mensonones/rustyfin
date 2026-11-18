#!/bin/sh
# Instala o Rustyfin como serviço do systemd (Linux). Precisa de root.
#
#   sudo sh packaging/linux/instalar.sh [caminho/do/rustyfin]      instala ou atualiza
#   sudo sh packaging/linux/instalar.sh --desinstalar              remove o serviço e o binário (mantém os dados)
#   sudo sh packaging/linux/instalar.sh --desinstalar --apagar-dados
#
# Binário: o baixado da página de versões (um arquivo chamado `rustyfin`) ou o de `cargo build --release`.
# Ainda não foi exercitado em todas as distribuições: relate problemas numa issue.
set -eu

AQUI=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN_DESTINO=/usr/local/bin/rustyfin
DADOS=/var/lib/rustyfin
CONFIG=/etc/rustyfin
UNIDADE=/etc/systemd/system/rustyfin.service

if [ "$(id -u)" -ne 0 ]; then
    echo "Rode como root: sudo sh $0 $*" >&2
    exit 1
fi
if ! command -v systemctl >/dev/null 2>&1; then
    echo "Este instalador usa o systemd. Sem ele, rode o binário direto (veja docs/instalacao-nativa.md)." >&2
    exit 1
fi

if [ "${1:-}" = "--desinstalar" ]; then
    systemctl disable --now rustyfin 2>/dev/null || true
    rm -f "$UNIDADE" "$BIN_DESTINO"
    systemctl daemon-reload
    if [ "${2:-}" = "--apagar-dados" ]; then
        rm -rf "$DADOS" "$CONFIG"
        echo "Serviço, binário e dados removidos."
    else
        echo "Serviço e binário removidos. Dados mantidos em $DADOS e a configuração em $CONFIG."
    fi
    exit 0
fi

BIN_ORIGEM=${1:-./rustyfin}
if [ ! -x "$BIN_ORIGEM" ]; then
    echo "Binário não encontrado ou sem permissão de execução: $BIN_ORIGEM" >&2
    echo "Baixe-o da página de versões ou compile com 'cargo build --release' (target/release/rustyfin)." >&2
    exit 1
fi

# Usuário de sistema sem login, dono dos dados. Precisa conseguir LER as suas pastas de mídia.
if ! id rustyfin >/dev/null 2>&1; then
    useradd --system --home-dir "$DADOS" --shell /usr/sbin/nologin rustyfin
fi
# GPU: o grupo que protege /dev/dri varia entre distribuições (render, video). Adiciona os que existirem.
for grupo in render video; do
    if getent group "$grupo" >/dev/null 2>&1; then
        usermod -aG "$grupo" rustyfin
    fi
done

systemctl stop rustyfin 2>/dev/null || true
install -m 0755 "$BIN_ORIGEM" "$BIN_DESTINO"
# Instalações antigas (antes da 0.25.4) deixaram a pasta em 0755 e o banco em 0644: fecha as permissões agora.
MODO_ANTES=$(stat -c %a "$DADOS" 2>/dev/null || echo "")
install -d -o rustyfin -g rustyfin -m 0750 "$DADOS"
chown -R rustyfin:rustyfin "$DADOS"
chmod 0750 "$DADOS"
find "$DADOS" -type f -exec chmod 0600 {} +
find "$DADOS" -mindepth 1 -type d -exec chmod 0700 {} +
if [ -n "$MODO_ANTES" ] && [ "$MODO_ANTES" != "750" ] && [ "$MODO_ANTES" != "700" ] && [ -f "$DADOS/rustyfin.db" ]; then
    echo "Aviso: a pasta de dados estava legível por outras contas deste computador, e o banco guarda tokens de sessão."
    echo "  Encerre as sessões em /admin -> Sessões (Encerrar todas) e troque as senhas, por precaução."
fi
install -d -m 0755 "$CONFIG"
if [ ! -f "$CONFIG/rustyfin.env" ]; then
    install -m 0640 -o root -g rustyfin "$AQUI/../rustyfin.env.example" "$CONFIG/rustyfin.env"
    echo "Configuração criada em $CONFIG/rustyfin.env (edite e reinicie: systemctl restart rustyfin)."
fi
install -m 0644 "$AQUI/rustyfin.service" "$UNIDADE"
systemctl daemon-reload
systemctl enable --now rustyfin

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "Aviso: ffmpeg não encontrado. Sem ele só há Direct Play." >&2
    echo "  Debian/Ubuntu: sudo apt install ffmpeg   Fedora: sudo dnf install ffmpeg" >&2
fi

IP=$(hostname -I 2>/dev/null | awk '{print $1}')
echo "Rustyfin instalado e em execução."
echo "  Painel: http://${IP:-localhost}:8096/admin"
echo "  Logs:   journalctl -u rustyfin -f"
