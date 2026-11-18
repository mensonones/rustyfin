#!/bin/sh
# Instala o Rustyfin como agente do launchd no macOS (sobe quando você entra na conta e reinicia se cair).
#
#   sh packaging/macos/instalar.sh [caminho/do/rustyfin]       instala ou atualiza
#   sh packaging/macos/instalar.sh --desinstalar               remove o agente e o binário (mantém os dados)
#   sh packaging/macos/instalar.sh --desinstalar --apagar-dados
#
# Binário: o baixado da página de versões (um arquivo chamado `rustyfin`) ou o de `cargo build --release`.
# Ainda não foi exercitado num Mac real: relate problemas numa issue.
set -eu

AQUI=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PASTA="$HOME/Library/Application Support/Rustyfin"
AGENTE="$HOME/Library/LaunchAgents/dev.rustyfin.plist"
ROTULO=dev.rustyfin
UID_ATUAL=$(id -u)

if [ "${1:-}" = "--desinstalar" ]; then
    launchctl bootout "gui/$UID_ATUAL/$ROTULO" 2>/dev/null || true
    rm -f "$AGENTE" "$PASTA/rustyfin" "$PASTA/iniciar.sh"
    if [ "${2:-}" = "--apagar-dados" ]; then
        rm -rf "$PASTA"
        echo "Agente, binário e dados removidos."
    else
        echo "Agente e binário removidos. Dados e configuração mantidos em $PASTA."
    fi
    exit 0
fi

BIN_ORIGEM=${1:-./rustyfin}
if [ ! -x "$BIN_ORIGEM" ]; then
    echo "Binário não encontrado ou sem permissão de execução: $BIN_ORIGEM" >&2
    echo "Baixe-o da página de versões ou compile com 'cargo build --release' (target/release/rustyfin)." >&2
    exit 1
fi

mkdir -p "$PASTA/dados" "$HOME/Library/LaunchAgents"
launchctl bootout "gui/$UID_ATUAL/$ROTULO" 2>/dev/null || true
cp "$BIN_ORIGEM" "$PASTA/rustyfin"
chmod 755 "$PASTA/rustyfin"
# Binários baixados vêm marcados pelo Gatekeeper; sem isso o macOS bloqueia a execução pelo agente.
xattr -d com.apple.quarantine "$PASTA/rustyfin" 2>/dev/null || true

if [ ! -f "$PASTA/rustyfin.env" ]; then
    cp "$AQUI/../rustyfin.env.example" "$PASTA/rustyfin.env"
    echo "Configuração criada em $PASTA/rustyfin.env (edite e rode este instalador de novo para reiniciar)."
fi

# O launchd começa com um PATH mínimo: o script acrescenta onde o Homebrew instala o ffmpeg, lê a configuração e inicia.
cat > "$PASTA/iniciar.sh" <<EOF
#!/bin/sh
export PATH="/opt/homebrew/bin:/usr/local/bin:\$PATH"
export RUSTYFIN_DATA_DIR="$PASTA/dados"
set -a
[ -f "$PASTA/rustyfin.env" ] && . "$PASTA/rustyfin.env"
set +a
exec "$PASTA/rustyfin"
EOF
chmod 755 "$PASTA/iniciar.sh"

cat > "$AGENTE" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$ROTULO</string>
    <key>ProgramArguments</key><array><string>$PASTA/iniciar.sh</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>$PASTA/rustyfin.log</string>
    <key>StandardErrorPath</key><string>$PASTA/rustyfin.log</string>
</dict>
</plist>
EOF

launchctl bootstrap "gui/$UID_ATUAL" "$AGENTE"

if ! PATH="/opt/homebrew/bin:/usr/local/bin:$PATH" command -v ffmpeg >/dev/null 2>&1; then
    echo "Aviso: ffmpeg não encontrado. Sem ele só há Direct Play. Instale com: brew install ffmpeg" >&2
fi
echo "Rustyfin instalado e em execução."
echo "  Painel: http://localhost:8096/admin"
echo "  Logs:   tail -f \"$PASTA/rustyfin.log\""
