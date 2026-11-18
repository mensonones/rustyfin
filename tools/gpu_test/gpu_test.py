#!/usr/bin/env python3
"""Testes do transcode por GPU: sobe o Rustyfin (fora do Docker), gera vídeos de teste com o ffmpeg e confere HLS,
formatos, legendas de imagem, ABR, avanço, sessões simultâneas e a volta automática para software.

Veja tools/gpu_test/README.md. Usa só a biblioteca padrão do Python e o `bench.py` (tools/bench).
"""
import argparse
import json
import os
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "bench"))
import bench  # noqa: E402  (Http, Server, PROFILE_HLS: o cliente HTTP e o perfil de reprodução)

WINDOWS = os.name == "nt"
OK, FAIL, SKIP = "OK    ", "FALHOU", "PULOU "

# rótulo -> (palavra que identifica o item, opções de vídeo, extensão). O servidor corta o nome do filme no primeiro
# termo técnico (H264, HEVC, 4K, AV1...), então a palavra vem antes dele e é com ela que o item é procurado.
VIDEO = {
    "H264 8 bits": ("Alfa", "-c:v libx264 -preset ultrafast -pix_fmt yuv420p", "mp4"),
    "H264 10 bits": ("Bravo", "-c:v libx264 -preset ultrafast -pix_fmt yuv420p10le", "mkv"),
    "HEVC 8 bits": ("Charlie", "-c:v libx265 -preset ultrafast -pix_fmt yuv420p -x265-params log-level=error", "mp4"),
    "HEVC 10 bits": ("Delta", "-c:v libx265 -preset ultrafast -pix_fmt yuv420p10le -x265-params log-level=error", "mp4"),
    "HEVC HDR10": (
        "Echo", "-c:v libx265 -preset ultrafast -pix_fmt yuv420p10le -x265-params "
        "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:hdr10=1:repeat-headers=1 "
        "-color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc", "mkv"),
    "VP9": ("Foxtrot", "-c:v libvpx-vp9 -deadline realtime -cpu-used 8 -pix_fmt yuv420p", "mkv"),
    "alto bitrate": ("Golf", "-vf noise=alls=30:allf=t -c:v libx264 -preset ultrafast -b:v 8M -pix_fmt yuv420p", "mp4"),
}
EXTRA = {  # rótulo -> (palavra, entrada lavfi, opções de vídeo, extensão)
    "4K H264": ("Hotel", "testsrc2=s=3840x2160:r=24:d=12", "-c:v libx264 -preset ultrafast -pix_fmt yuv420p", "mp4"),
    "4K HEVC": ("India", "testsrc2=s=3840x2160:r=24:d=12", "-c:v libx265 -preset ultrafast -pix_fmt yuv420p -x265-params log-level=error", "mp4"),
    "AV1": ("Juliet", "testsrc2=s=1920x1080:r=24:d=12", "-c:v libsvtav1 -preset 12 -crf 38 -pix_fmt yuv420p", "mkv"),
    "Entrelacado": ("Kilo", "testsrc2=s=1920x1080:r=25:d=12", "-vf interlace=scan=tff -flags +ilme+ildct -x264opts tff=1 -c:v libx264 -preset ultrafast -pix_fmt yuv420p", "mp4"),
}
PALAVRA = {**{k: v[0] for k, v in VIDEO.items()}, **{k: v[0] for k, v in EXTRA.items()}}
LEGENDA = {  # vídeos com uma faixa PGS (retângulo branco); o nome já é a palavra de busca
    "Legenda Alfa": "-c:v libx264 -preset ultrafast",
    "Legenda Beta": "-c:v libx265 -preset ultrafast -x265-params log-level=error",
}
RECT = (700, 900, 520, 90)  # x, y, largura, altura do retângulo da legenda


# ------------------------------------------------------------------ ferramentas e geração de arquivos
class Ferramentas:
    def __init__(self, ffmpeg_dir):
        exe = ".exe" if WINDOWS else ""
        if ffmpeg_dir:
            self.ffmpeg = str(Path(ffmpeg_dir) / f"ffmpeg{exe}")
            self.ffprobe = str(Path(ffmpeg_dir) / f"ffprobe{exe}")
            self.dir = str(Path(ffmpeg_dir))
        else:
            self.ffmpeg = shutil.which("ffmpeg") or "ffmpeg"
            self.ffprobe = shutil.which("ffprobe") or "ffprobe"
            self.dir = str(Path(self.ffmpeg).parent) if shutil.which("ffmpeg") else ""

    def run(self, args, **kw):
        return subprocess.run([self.ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-y", *args], capture_output=True, text=True, **kw)


def pg_segmento(tipo, pts, payload):
    return b"PG" + struct.pack(">II", int(pts * 90000), 0) + bytes([tipo]) + struct.pack(">H", len(payload)) + payload


def gerar_sup(caminho, w=1920, h=1080, rect=RECT, mostra=1.0, esconde=9.0):
    """Escreve um arquivo PGS (legenda de imagem do Blu-ray) com um retângulo branco opaco de `mostra` a `esconde` s."""
    x, y, rw, rh = rect
    linha = bytes([0x00, 0xC0 | (rw >> 8), rw & 0xFF, 0x01]) + b"\x00\x00"  # cor 1 por rw pixels + fim de linha
    rle = linha * rh
    pcs = struct.pack(">HHBHBBBB", w, h, 0x10, 1, 0x80, 0, 0, 1) + struct.pack(">HBBHH", 0, 0, 0, x, y)
    wds = bytes([1, 0]) + struct.pack(">HHHH", x, y, rw, rh)
    pds = bytes([0, 0]) + bytes([0, 16, 128, 128, 0]) + bytes([1, 235, 128, 128, 255])
    ods = struct.pack(">HBB", 0, 0, 0xC0) + (len(rle) + 4).to_bytes(3, "big") + struct.pack(">HH", rw, rh) + rle
    out = b"".join(pg_segmento(t, mostra, p) for t, p in ((0x16, pcs), (0x17, wds), (0x14, pds), (0x15, ods), (0x80, b"")))
    pcs2 = struct.pack(">HHBHBBBB", w, h, 0x10, 2, 0x00, 0, 0, 0)
    out += b"".join(pg_segmento(t, esconde, p) for t, p in ((0x16, pcs2), (0x17, bytes([1, 0]) + struct.pack(">HHHH", x, y, rw, rh)), (0x80, b"")))
    Path(caminho).write_bytes(out)


def gerar_videos(ft, pasta, quais):
    """Gera (uma vez) os vídeos de teste em `pasta`. Devolve os nomes que existem no fim."""
    pasta.mkdir(parents=True, exist_ok=True)
    som = ["-f", "lavfi", "-i", "sine=f=440:d=40"]
    sup = pasta.parent / "legenda.sup"
    gerar_sup(sup)
    existentes = []

    def feito(rotulo, arquivo, ext, args):
        dest = pasta / f"{arquivo} (2024).{ext}"
        if dest.exists():
            existentes.append(rotulo)
            return
        r = ft.run(args + [str(dest)])
        if r.returncode == 0 and dest.exists():
            print(f"  gerado  {dest.name}")
            existentes.append(rotulo)
        else:
            dest.unlink(missing_ok=True)
            print(f"  não gerado {dest.name} (o ffmpeg não tem o encoder?): {r.stderr.strip()[-90:]}")

    if "formatos" in quais:
        for rotulo, (palavra, opts, ext) in VIDEO.items():
            feito(rotulo, f"{palavra} {rotulo}", ext, ["-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=24:d=40", *som, *opts.split(), "-c:a", "aac", "-b:a", "128k", "-shortest"])
    if "extra" in quais:
        for rotulo, (palavra, src, opts, ext) in EXTRA.items():
            feito(rotulo, f"{palavra} {rotulo}", ext, ["-f", "lavfi", "-i", src, *som, *opts.split(), "-c:a", "aac", "-shortest"])
        for nome, cod in LEGENDA.items():
            feito(nome, nome, "mkv", ["-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=24:d=20", "-f", "lavfi", "-i", "sine=f=440:d=20", "-i", str(sup),
                                      "-map", "0:v", "-map", "1:a", "-map", "2:s", *cod.split(), "-pix_fmt", "yuv420p", "-c:a", "aac", "-c:s", "copy", "-shortest"])
    return existentes


# ------------------------------------------------------------------ medição de CPU (sistema inteiro)
def tempos_cpu():
    if WINDOWS:
        import ctypes
        from ctypes import wintypes
        idle, kern, user = (wintypes.FILETIME(), wintypes.FILETIME(), wintypes.FILETIME())
        ctypes.windll.kernel32.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kern), ctypes.byref(user))
        f = lambda t: (t.dwHighDateTime << 32) | t.dwLowDateTime  # noqa: E731
        return f(idle), f(kern) + f(user)  # no Windows `kern` já inclui o tempo ocioso
    try:
        v = [int(x) for x in Path("/proc/stat").read_text().splitlines()[0].split()[1:]]
        return v[3] + v[4], sum(v)
    except (OSError, ValueError, IndexError):
        return 0, 0


def cpu_pct(a, b):
    total = b[1] - a[1]
    return 100.0 * (1 - (b[0] - a[0]) / total) if total > 0 else float("nan")


# ------------------------------------------------------------------ servidor sob teste
class Servidor:
    def __init__(self, exe, ft, pasta, esperados, encoder, porta, env_extra=None):
        self.tmp = tempfile.mkdtemp(prefix="rf-gpu-")
        env = dict(os.environ, RUSTYFIN_FFMPEG=ft.ffmpeg, RUSTYFIN_VIDEO_ENCODER=encoder, RUSTYFIN_PORT=str(porta), RUSTYFIN_HOST="127.0.0.1",
                   RUSTYFIN_MAX_TRANSCODES="6", DATABASE_URL="sqlite://" + os.path.join(self.tmp, "rf.db").replace("\\", "/") + "?mode=rwc",
                   RUSTYFIN_MEDIA_DIRS=os.path.join(self.tmp, "vazio"), RUSTYFIN_CACHE_DIR=os.path.join(self.tmp, "cache"),
                   RUSTYFIN_DISCOVERY="false", RUSTYFIN_WATCH="false", RUSTYFIN_SCAN_INTERVAL_SECS="0", RUST_LOG="rustyfin=info,tower_http=off")
        if ft.dir:  # o scanner chama o `ffprobe` pelo PATH
            env["PATH"] = ft.dir + os.pathsep + env.get("PATH", "")
        env.update(env_extra or {})
        os.makedirs(env["RUSTYFIN_MEDIA_DIRS"], exist_ok=True)
        self.porta = porta
        self.logpath = os.path.join(self.tmp, "server.log")
        self.logf = open(self.logpath, "w", encoding="utf-8")
        self.proc = subprocess.Popen([exe], env=env, stdout=self.logf, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, cwd=self.tmp)
        self.srv = bench.Server("rustyfin", porta, "folgado", {})
        self.srv.wait_http(60)
        self.srv.setup()
        self.srv.add_libraries([("Filmes", "movies", str(pasta))])
        self.srv.wait_count(esperados, time.perf_counter(), 180)
        self.srv.wait_idle(time.perf_counter(), 60)
        time.sleep(2)

    @property
    def http(self):
        return self.srv.http

    def log(self):
        self.logf.flush()
        return Path(self.logpath).read_text(encoding="utf-8", errors="replace")

    def problemas_desde(self, n, ignorar=()):
        linhas = self.log()[n:].splitlines()
        chaves = ("rror", "nvalid", "ailed", "falhou", "WARN")
        return [l.strip()[:200] for l in linhas if any(k in l for k in chaves) and "miniatura" not in l and not any(i in l for i in ignorar)]

    def parar(self):
        self.proc.kill()
        self.logf.close()
        time.sleep(1)
        shutil.rmtree(self.tmp, ignore_errors=True)


def achar(s, rotulo):
    nome = PALAVRA.get(rotulo, rotulo)
    for _ in range(40):
        it = s.srv.find_item(nome)
        if it:
            return it
        time.sleep(0.5)
    raise RuntimeError(f"item {rotulo!r} (busca {nome!r}) não encontrado na biblioteca")


# ------------------------------------------------------------------ HLS
def playlist(s, it, abr=False, bitrate=None, sub_idx=None):
    h = s.http
    bit = bitrate or bench.FORCE_BITRATE
    body = {"DeviceProfile": dict(bench.PROFILE_HLS, MaxStreamingBitrate=bit), "EnableDirectPlay": False, "EnableDirectStream": False,
            "EnableTranscoding": True, "UserId": h.uid, "MaxStreamingBitrate": bit}
    if sub_idx is not None:
        body["SubtitleStreamIndex"] = sub_idx
    c, v, _ = h.req(f"/Items/{it}/PlaybackInfo?UserId={h.uid}&MediaSourceId={it}", "POST", body)
    url = ((v or {}).get("MediaSources") or [{}])[0].get("TranscodingUrl")
    if not url:
        raise RuntimeError(f"sem TranscodingUrl (HTTP {c})")
    if not abr:
        url = url.replace("&abr=true", "").replace("abr=true&", "")
    c, master, _ = h.req(url + f"&api_key={h.token}", raw=True)
    base = url.split("?")[0].rsplit("/", 1)[0]
    variantes = [(l if l.startswith("/") else base + "/" + l) for l in master.decode().splitlines() if l and not l.startswith("#")]
    return url, variantes, v["PlaySessionId"]


def baixar_segmento(s, variante, indice):
    h = s.http
    sep = "&" if "?" in variante else "?"
    c, pl, _ = h.req(variante + sep + f"api_key={h.token}", raw=True)
    segs = [l for l in pl.decode().splitlines() if l and not l.startswith("#")]
    base = variante.split("?")[0].rsplit("/", 1)[0]
    su = segs[indice] if segs[indice].startswith("/") else base + "/" + segs[indice]
    t = time.perf_counter()
    code, d, _ = h.req(su + ("&" if "?" in su else "?") + f"api_key={h.token}", raw=True, timeout=120)
    return code, d, time.perf_counter() - t, len(segs)


def parar_sessao(s, psid, it):
    s.http.req("/Sessions/Playing/Stopped", "POST", {"ItemId": it, "PositionTicks": 0, "PlaySessionId": psid})


def sondar(ft, dados, sufixo=".ts"):
    f = os.path.join(tempfile.gettempdir(), "rf-gpu-seg" + sufixo)
    Path(f).write_bytes(dados)
    r = subprocess.run([ft.ffprobe, "-v", "error", "-show_entries", "format=duration:stream=codec_name,pix_fmt", "-of", "json", f], capture_output=True, text=True)
    try:
        j = json.loads(r.stdout)
        return float(j["format"]["duration"]), j["streams"][0].get("codec_name")
    except (ValueError, KeyError, IndexError):
        return None, None


def sessao(s, it, nseg, **kw):
    """Baixa `nseg` segmentos em sequência e devolve (velocidade em × tempo real, tempo do 1º segmento)."""
    url, variantes, psid = playlist(s, it, **kw)
    t0 = time.perf_counter()
    primeiro = None
    for i in range(nseg):
        code, d, _, _ = baixar_segmento(s, variantes[0], i)
        if code != 200 or len(d) < 1000:
            raise RuntimeError(f"segmento {i} falhou (HTTP {code})")
        primeiro = primeiro or time.perf_counter() - t0
    total = time.perf_counter() - t0
    parar_sessao(s, psid, it)
    return nseg * 6 / total, primeiro


def media_regiao(ft, ts_bytes, t=2.0):
    f = os.path.join(tempfile.gettempdir(), "rf-gpu-quadro.ts")
    Path(f).write_bytes(ts_bytes)
    x, y, w, h = RECT
    r = subprocess.run([ft.ffmpeg, "-v", "error", "-i", f, "-ss", str(t), "-frames:v", "1", "-vf", f"crop={w}:{h}:{x}:{y},format=gray", "-f", "rawvideo", "-"], capture_output=True)
    if not r.stdout:
        raise RuntimeError("o ffmpeg não extraiu o quadro: " + r.stderr.decode(errors="replace")[-150:])
    return sum(r.stdout) / len(r.stdout)


# ------------------------------------------------------------------ casos
class Resultado:
    def __init__(self):
        self.falhas = 0
        self.total = 0

    def caso(self, nome, fn, existe=True, motivo="arquivo de teste não gerado"):
        self.total += 1
        if not existe:
            print(f"  {SKIP} {nome}: {motivo}")
            return
        try:
            print(f"  {OK} {nome}: {fn()}")
        except Exception as e:  # noqa: BLE001 - qualquer falha de um caso vira linha de relatório
            self.falhas += 1
            print(f"  {FAIL} {nome}: {str(e)[:240]}")


def suite_formatos(s, ft, r, tem):
    print("\n== Formatos de origem (HLS, 3 segmentos de 6 s)")
    for nome in VIDEO:
        def f(nome=nome):
            n = len(s.log())
            x, primeiro = sessao(s, achar(s, nome), 3, abr=False)
            erros = s.problemas_desde(n)
            if erros:
                raise RuntimeError(" | ".join(erros[:2]))
            return f"{x:.1f}x tempo real, 1º segmento {primeiro:.2f}s"
        r.caso(nome, f, nome in tem)

    base = "H264 8 bits"
    print("\n== Comportamento do HLS")
    def segmentos():
        it = achar(s, base)
        url, v, psid = playlist(s, it)
        durs = [round(sondar(ft, baixar_segmento(s, v[0], i)[1])[0] or 0, 2) for i in range(3)]
        parar_sessao(s, psid, it)
        if any(abs(d - 6.0) > 0.25 for d in durs):
            raise RuntimeError(f"segmentos fora de 6 s: {durs}")
        return f"durações {durs}"
    r.caso("segmentos de 6 s exatos", segmentos, base in tem)

    def avanco():
        n = len(s.log())
        it = achar(s, base)
        url, v, psid = playlist(s, it)
        c5, d5, t5, _ = baixar_segmento(s, v[0], 5)
        c1, d1, t1, _ = baixar_segmento(s, v[0], 1)
        parar_sessao(s, psid, it)
        erros = s.problemas_desde(n)
        if c5 != 200 or c1 != 200 or erros:
            raise RuntimeError(f"HTTP {c5}/{c1} {erros[:1]}")
        return f"segmento 5 direto em {t5:.1f}s, volta ao 1 em {t1:.1f}s"
    r.caso("avanço (seek)", avanco, base in tem)

    def abr():
        it = achar(s, "alto bitrate")
        url, v, psid = playlist(s, it, abr=True, bitrate=6_000_000)
        res = [baixar_segmento(s, var, 0)[0] for var in v]
        parar_sessao(s, psid, it)
        if len(v) < 2 or any(c != 200 for c in res):
            raise RuntimeError(f"{len(v)} variante(s), HTTP {res}")
        return f"{len(v)} variantes OK"
    r.caso("ABR (2 qualidades)", abr, "alto bitrate" in tem)

    def fmp4():
        it = achar(s, "HEVC 8 bits")
        url, v, psid = playlist(s, it)
        h = s.http
        url2 = url + "&segmentContainer=mp4"
        c, master, _ = h.req(url2 + f"&api_key={h.token}", raw=True)
        base_url = url2.split("?")[0].rsplit("/", 1)[0]
        var = [l for l in master.decode().splitlines() if l and not l.startswith("#")][0]
        var = var if var.startswith("/") else base_url + "/" + var
        code, d, t, _ = baixar_segmento(s, var, 0)
        parar_sessao(s, psid, it)
        if code != 200 or len(d) < 1000:
            raise RuntimeError(f"HTTP {code}, {len(d)} bytes")
        return f"segmento fMP4 de {len(d) // 1024} KB"
    r.caso("segmentos fMP4", fmp4, "HEVC 8 bits" in tem)

    def progressivo():
        it = achar(s, base)
        h = s.http
        url = f"http://127.0.0.1:{s.porta}/Videos/{it}/stream.mkv?mediaSourceId={it}&transcode=full&audioCodec=aac&videoBitrate=700000&api_key={h.token}"
        dados = b""
        with urllib.request.urlopen(urllib.request.Request(url), timeout=60) as resp:
            while len(dados) < 1_500_000:
                bloco = resp.read(65536)
                if not bloco:
                    break
                dados += bloco
        if len(dados) < 100_000:
            raise RuntimeError(f"só {len(dados)} bytes")
        return f"{len(dados) // 1024} KB recebidos"
    r.caso("stream progressivo (MKV)", progressivo, base in tem)

    def simultaneas():
        it = achar(s, base)
        saidas, erros = [], []
        def w():
            try:
                saidas.append(sessao(s, it, 3)[0])
            except Exception as e:  # noqa: BLE001
                erros.append(str(e))
        t0 = tempos_cpu()
        ths = [threading.Thread(target=w) for _ in range(3)]
        [t.start() for t in ths]
        [t.join() for t in ths]
        cpu = cpu_pct(t0, tempos_cpu())
        if erros:
            raise RuntimeError(erros[0])
        return f"3/3 OK, {sum(saidas) / len(saidas):.1f}x cada, CPU do sistema ≈ {cpu:.0f}%"
    r.caso("3 sessões simultâneas", simultaneas, base in tem)


def suite_extra(s, ft, r, tem):
    print("\n== Formatos extras (HLS, 2 segmentos = 12 s)")
    for nome in EXTRA:
        def f(nome=nome):
            n = len(s.log())
            x, primeiro = sessao(s, achar(s, nome), 2)
            erros = s.problemas_desde(n)
            if erros:
                raise RuntimeError(" | ".join(erros[:2]))
            return f"{x:.1f}x tempo real"
        r.caso(nome, f, nome in tem)

    print("\n== Legenda de imagem (PGS) queimada: retângulo branco de 0 a 8 s")
    for nome in LEGENDA:
        def f(nome=nome):
            n = len(s.log())
            it = achar(s, nome)
            c, d, _ = s.http.req(s.http.u(f"/Items/{it}?Fields=MediaSources"))
            streams = (d.get("MediaSources") or [{}])[0].get("MediaStreams", [])
            idx = next((x["Index"] for x in streams if x.get("Type") == "Subtitle"), None)
            if idx is None:
                raise RuntimeError("o servidor não listou a faixa de legenda")
            res = []
            for sub in (idx, None):
                url, v, psid = playlist(s, it, sub_idx=sub)
                code, dados, _, _ = baixar_segmento(s, v[0], 0)
                parar_sessao(s, psid, it)
                if code != 200:
                    raise RuntimeError(f"HTTP {code}")
                res.append(media_regiao(ft, dados))
            erros = s.problemas_desde(n)
            if erros:
                raise RuntimeError(" | ".join(erros[:2]))
            if not (res[0] > 200 and res[0] - res[1] > 40):
                raise RuntimeError(f"a legenda não apareceu no vídeo (região: com={res[0]:.0f}, sem={res[1]:.0f})")
            return f"legenda queimada (região {res[0]:.0f} com legenda, {res[1]:.0f} sem)"
        r.caso(nome, f, nome in tem)


def criar_wrapper_que_falha(ft, pasta):
    """Um `ffmpeg` que falha em sessões reais com h264_qsv/h264_vaapi (as sondas da subida, com -frames:v, passam)."""
    if WINDOWS:
        arq = pasta / "ffmpeg_falha_gpu.bat"
        arq.write_text(
            "@echo off\r\n"
            'echo %* | findstr /C:"-frames:v" >nul\r\nif %errorlevel%==0 goto real\r\n'
            'echo %* | findstr /C:"h264_qsv" >nul\r\nif %errorlevel%==0 (\r\n  echo [simulado] falha da GPU 1>&2\r\n  exit /b 1\r\n)\r\n'
            'echo %* | findstr /C:"h264_vaapi" >nul\r\nif %errorlevel%==0 (\r\n  echo [simulado] falha da GPU 1>&2\r\n  exit /b 1\r\n)\r\n'
            f':real\r\n"{ft.ffmpeg}" %*\r\nexit /b %errorlevel%\r\n')
    else:
        arq = pasta / "ffmpeg_falha_gpu.sh"
        arq.write_text(
            "#!/bin/sh\n"
            'case "$*" in *-frames:v*) exec "' + ft.ffmpeg + '" "$@";; esac\n'
            'case "$*" in *h264_qsv*|*h264_vaapi*) echo "[simulado] falha da GPU" >&2; exit 1;; esac\n'
            'exec "' + ft.ffmpeg + '" "$@"\n')
        arq.chmod(arq.stat().st_mode | stat.S_IEXEC)
    return str(arq)


def suite_falha(args, ft, pasta, esperados, r):
    print("\n== Volta automática para software (ffmpeg que falha com a GPU)")
    wrapper = criar_wrapper_que_falha(ft, Path(tempfile.mkdtemp(prefix="rf-gpu-wrap-")))
    s = Servidor(args.exe, ft, pasta, esperados, args.encoder, args.porta, {"RUSTYFIN_FFMPEG": wrapper})
    try:
        usa_gpu = any("ardware ativa" in l for l in s.log().splitlines())
        def f():
            it = achar(s, "H264 8 bits")
            x, _ = sessao(s, it, 3)
            n_fallback = sum("refazendo em software" in l for l in s.log().splitlines())
            if n_fallback < 1:
                raise RuntimeError("a sessão funcionou, mas o log não registrou a volta para software")
            return f"sessão concluída em software ({x:.1f}x); {n_fallback} aviso(s) de volta para software no log"
        if not usa_gpu:
            r.caso("GPU que falha", f, existe=False, motivo="a GPU não está ativa neste servidor, não há falha para simular")
        else:
            r.caso("GPU que falha", f, "H264 8 bits" in args.tem)
    finally:
        s.parar()


# ------------------------------------------------------------------ main
def main():
    ap = argparse.ArgumentParser(description="Testes do transcode por GPU (veja tools/gpu_test/README.md).")
    raiz = HERE.parent.parent
    padrao = next((str(p) for p in (raiz / "target" / "release" / ("rustyfin.exe" if WINDOWS else "rustyfin"), raiz / "target" / "debug" / ("rustyfin.exe" if WINDOWS else "rustyfin")) if p.exists()), "")
    ap.add_argument("--exe", default=padrao, help="binário do Rustyfin (padrão: target/release ou target/debug)")
    ap.add_argument("--ffmpeg-dir", default="", help="pasta com ffmpeg e ffprobe (padrão: os do PATH)")
    ap.add_argument("--encoder", default="auto", help="RUSTYFIN_VIDEO_ENCODER: auto (padrão), libx264, h264_qsv, h264_vaapi")
    ap.add_argument("--suite", default="todos", choices=["formatos", "extra", "falha", "todos"])
    ap.add_argument("--porta", type=int, default=18140)
    ap.add_argument("--clips-dir", default=str(Path(tempfile.gettempdir()) / "rustyfin-gpu-test"), help="onde guardar os vídeos de teste (reaproveitados)")
    ap.add_argument("--limpar", action="store_true", help="apaga os vídeos de teste no fim")
    args = ap.parse_args()
    if not args.exe or not Path(args.exe).exists():
        sys.exit("Binário do Rustyfin não encontrado: compile com `cargo build --release` ou passe --exe.")
    args.exe = str(Path(args.exe).resolve())  # o servidor roda em outra pasta (cwd temporário)

    ft = Ferramentas(args.ffmpeg_dir)
    quais = {"formatos", "extra"} if args.suite in ("todos", "falha") else {args.suite}
    pasta = Path(args.clips_dir) / "filmes"
    print(f"Gerando vídeos de teste em {pasta} (reaproveita os que já existem)...")
    tem = set(gerar_videos(ft, pasta, quais))
    args.tem = tem
    if not tem:
        sys.exit("Nenhum vídeo de teste pôde ser gerado: o ffmpeg precisa de libx264.")

    r = Resultado()
    s = Servidor(args.exe, ft, pasta, len(tem), args.encoder, args.porta)
    try:
        ativo = [l.split("INFO ")[-1].strip() for l in s.log().splitlines() if "ardware ativa" in l or "ffmpeg encontrado" in l]
        print("\nServidor:", ativo[0][:140] if ativo else "(sem linha de encoder)")
        if args.suite in ("formatos", "todos"):
            suite_formatos(s, ft, r, tem)
        if args.suite in ("extra", "todos"):
            suite_extra(s, ft, r, tem)
    finally:
        s.parar()
    if args.suite in ("falha", "todos"):
        suite_falha(args, ft, pasta, len(tem), r)
    if args.limpar:
        shutil.rmtree(args.clips_dir, ignore_errors=True)
    print(f"\n{r.total - r.falhas}/{r.total} casos sem falha." + ("" if not r.falhas else f" {r.falhas} FALHARAM."))
    return 1 if r.falhas else 0


if __name__ == "__main__":
    sys.exit(main())
