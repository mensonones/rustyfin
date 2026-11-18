#!/bin/sh
# Diagnóstico da aceleração por GPU do Rustyfin, para rodar DENTRO do container:
#
#   docker compose exec rustyfin sh /app/gpu-check.sh
#
# Testa, na ordem: o dispositivo da GPU e a permissão, o `vainfo`, a codificação VAAPI, a decodificação VAAPI e a
# velocidade contra o libx264. No fim imprime um resumo que dá para colar numa issue. Não altera nada.
FFMPEG="${RUSTYFIN_FFMPEG:-ffmpeg}"
DEV="${RUSTYFIN_HWACCEL_DEVICE:-/dev/dri/renderD128}"
TMP="${TMPDIR:-/tmp}/rustyfin-gpucheck-$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

R_DEV="não"; R_PERM="não"; R_ENC="não"; R_DEC="não"; R_SW=""; R_HW=""; R_DRV="?"

ok()   { echo "  [ok]    $*"; }
fail() { echo "  [FALHA] $*"; }
info() { echo "  [info]  $*"; }

now() { date +%s.%N; }
speed() { # $1 = segundos de vídeo, $2 = início, $3 = fim
  awk -v d="$1" -v a="$2" -v b="$3" 'BEGIN { if (b > a) printf "%.1f", d / (b - a); else print "?" }'
}

echo "== Rustyfin: diagnóstico da GPU =="
echo "ffmpeg:  $("$FFMPEG" -hide_banner -version 2>/dev/null | head -n 1)"
echo "usuário: $(id)"
echo "kernel:  $(uname -r)"

echo
echo "1) Dispositivo da GPU"
if [ -d /dev/dri ]; then
  ls -l /dev/dri | sed 's/^/        /'
else
  fail "/dev/dri não existe dentro do container."
  if [ -e /dev/dxg ]; then
    info "Há /dev/dxg: isto é o Docker Desktop (WSL2). Ele não repassa a GPU Intel ao container; veja docs/reproducao.md#windows."
  else
    info "No Linux, repasse a GPU: rode 'sh tools/gpu-setup.sh' no host e 'docker compose up -d'."
  fi
fi
if [ -e "$DEV" ]; then
  R_DEV="sim"
  ok "$DEV existe."
  if [ -r "$DEV" ] && [ -w "$DEV" ]; then
    R_PERM="sim"
    ok "o processo pode abrir $DEV."
  else
    fail "sem permissão para abrir $DEV (grupo dono: $(stat -c %g "$DEV")). Adicione 'group_add' ao serviço."
  fi
else
  fail "$DEV não existe."
fi

echo
echo "2) vainfo (driver e perfis)"
if [ "$R_PERM" = "sim" ] && command -v vainfo >/dev/null 2>&1; then
  out=$(vainfo --display drm --device "$DEV" 2>&1)
  echo "$out" | grep -E "Driver version|VAProfileH264(Main|High)|VAProfileHEVCMain" | sed 's/^/        /' | head -n 8
  R_DRV=$(echo "$out" | grep -m1 "Driver version" | sed 's/.*Driver version: //')
  [ -n "$R_DRV" ] || R_DRV="?"
else
  info "pulado (sem permissão no dispositivo ou vainfo ausente)."
fi

if [ "$R_PERM" != "sim" ]; then
  echo
  echo "== Resumo (cole numa issue) =="
  echo "dispositivo $DEV existe=$R_DEV, permissão=$R_PERM. Nada mais foi testado."
  echo "Rustyfin: encoder em uso = libx264 (software)."
  exit 1
fi

INIT="-init_hw_device vaapi=hw:$DEV -filter_hw_device hw"
echo
echo "3) Codificação na GPU (h264_vaapi)"
if "$FFMPEG" -nostdin -hide_banner -loglevel error $INIT -f lavfi -i "color=c=black:s=256x144:d=0.2:r=25" \
     -vf format=nv12,hwupload -c:v h264_vaapi -frames:v 1 -f null - 2>"$TMP/enc.err"; then
  R_ENC="sim"; ok "um quadro foi codificado com h264_vaapi."
else
  fail "h264_vaapi falhou: $(tail -n 1 "$TMP/enc.err")"
fi

echo
echo "4) Decodificação na GPU"
if [ "$R_ENC" = "sim" ] && "$FFMPEG" -nostdin -hide_banner -loglevel error -f lavfi -i "testsrc=s=640x360:d=1:r=25" \
     -c:v libx264 -pix_fmt yuv420p -y "$TMP/amostra.mp4" 2>/dev/null; then
  if "$FFMPEG" -nostdin -hide_banner -loglevel error $INIT -hwaccel vaapi -hwaccel_output_format vaapi -hwaccel_device hw \
       -i "$TMP/amostra.mp4" -frames:v 5 -vf scale_vaapi=format=nv12 -c:v h264_vaapi -f null - 2>"$TMP/dec.err"; then
    R_DEC="sim"; ok "H.264 decodificado e recodificado inteiramente na GPU."
  else
    fail "decodificação na GPU falhou: $(tail -n 1 "$TMP/dec.err")"
    info "o Rustyfin usaria a GPU só para codificar."
  fi
else
  info "pulado."
fi

echo
echo "5) Velocidade (20 s de vídeo 1080p sintético, 4 Mbps)"
SRC="testsrc2=s=1920x1080:r=24:d=20"
t0=$(now)
"$FFMPEG" -nostdin -hide_banner -loglevel error -f lavfi -i "$SRC" -c:v libx264 -preset veryfast -b:v 4M -f null - 2>/dev/null
t1=$(now)
R_SW=$(speed 20 "$t0" "$t1")
echo "        libx264 (CPU):   ${R_SW}x o tempo real"
if [ "$R_ENC" = "sim" ]; then
  t0=$(now)
  "$FFMPEG" -nostdin -hide_banner -loglevel error $INIT -f lavfi -i "$SRC" -vf format=nv12,hwupload \
    -c:v h264_vaapi -rc_mode VBR -b:v 4M -maxrate 4M -bufsize 8M -f null - 2>/dev/null
  t1=$(now)
  R_HW=$(speed 20 "$t0" "$t1")
  echo "        h264_vaapi (GPU): ${R_HW}x o tempo real"
fi

echo
echo "== Resumo (cole numa issue) =="
echo "dispositivo=$R_DEV permissão=$R_PERM driver=$R_DRV"
echo "codifica na GPU=$R_ENC decodifica na GPU=$R_DEC"
echo "velocidade: libx264=${R_SW}x h264_vaapi=$([ -n "$R_HW" ] && echo "${R_HW}x" || echo n/d)"
if [ "$R_ENC" = "sim" ]; then
  echo "Rustyfin: o encoder 'auto' usará h264_vaapi ($([ "$R_DEC" = "sim" ] && echo 'decodifica e codifica na GPU' || echo 'só codifica na GPU'))."
  exit 0
fi
echo "Rustyfin: o encoder 'auto' cairá para libx264 (software)."
exit 1
