#!/usr/bin/env python3
"""Mede, nesta máquina, qual perfil de execução do Rustyfin transcodifica melhor e recomenda um.

Perfis comparados (os que a máquina permitir):
  docker-cpu    container Docker, libx264 (processador)
  docker-gpu    container Docker com a GPU repassada (Linux: /dev/dri, VAAPI)
  nativo-cpu    ffmpeg direto no sistema, libx264
  nativo-gpu    ffmpeg direto no sistema com a GPU (Windows: Quick Sync; Linux: VAAPI)

O teste usa os mesmos argumentos de ffmpeg do servidor (veja `src/transcode/args.rs`): um vídeo H.264 de 1080p é reduzido
para 720p a 4 Mbps, decodificando e codificando na GPU quando o perfil é de GPU. Mede a velocidade (múltiplos do tempo
real), os núcleos de CPU ocupados e a memória não é medida. Usa só a biblioteca padrão.

    python tools/perfil/escolher_perfil.py                      # tudo o que a máquina permitir
    python tools/perfil/escolher_perfil.py --ffmpeg-dir C:\\ffmpeg\\bin   # Windows: ffmpeg com Quick Sync
    python tools/perfil/escolher_perfil.py --json               # só o JSON, para uma LLM ou um script ler
"""
import argparse
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

VERSAO_SCRIPT = 1
BITRATE = "4000000"
IMAGENS_PADRAO = ("rustyfin-rustyfin:latest", "rustyfin:latest")
WINDOWS = os.name == "nt"


def executar(cmd, timeout=300, **kw):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, errors="replace", **kw)
    except FileNotFoundError:
        return subprocess.CompletedProcess(cmd, 127, "", f"não encontrado: {cmd[0]}")
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(cmd, 124, "", "tempo esgotado")


def cauda(texto, n=200):
    texto = (texto or "").strip().replace("\n", " ")
    return texto[-n:]


# ---------------------------------------------------------------- ambiente

def nome_cpu():
    if WINDOWS:
        r = executar(["powershell", "-NoProfile", "-Command", "(Get-CimInstance Win32_Processor | Select-Object -First 1).Name"], 30)
        if r.returncode == 0 and r.stdout.strip():
            return r.stdout.strip()
    elif Path("/proc/cpuinfo").exists():
        for linha in Path("/proc/cpuinfo").read_text(errors="replace").splitlines():
            if linha.startswith("model name"):
                return linha.split(":", 1)[1].strip()
    return platform.processor() or platform.machine()


def placas_de_video():
    if WINDOWS:
        r = executar(["powershell", "-NoProfile", "-Command",
                      "Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name }"], 30)
        return [l.strip() for l in r.stdout.splitlines() if l.strip()]
    if shutil.which("lspci"):
        r = executar(["lspci"], 15)
        return [l.split(": ", 1)[-1].strip() for l in r.stdout.splitlines() if re.search(r"VGA|3D|Display", l)]
    return []


def memoria_gib():
    try:
        if WINDOWS:
            r = executar(["powershell", "-NoProfile", "-Command", "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"], 30)
            return round(int(r.stdout.strip()) / 2**30, 1)
        for linha in Path("/proc/meminfo").read_text().splitlines():
            if linha.startswith("MemTotal"):
                return round(int(linha.split()[1]) / 2**20, 1)
    except (ValueError, OSError):
        pass
    return None


def info_docker():
    r = executar(["docker", "info", "--format", "{{json .}}"], 30)
    if r.returncode != 0:
        return {"disponivel": False, "motivo": cauda(r.stderr) or "docker não encontrado"}
    try:
        d = json.loads(r.stdout)
    except json.JSONDecodeError:
        return {"disponivel": False, "motivo": "resposta inesperada do docker info"}
    return {
        "disponivel": True,
        "sistema": d.get("OperatingSystem"),
        "desktop": "Docker Desktop" in (d.get("OperatingSystem") or "") or "desktop" in (d.get("Name") or "").lower(),
        "cpus": d.get("NCPU"),
        "memoria_gib": round((d.get("MemTotal") or 0) / 2**30, 1),
    }


def achar_imagem(pedida):
    for nome in ([pedida] if pedida else list(IMAGENS_PADRAO)):
        r = executar(["docker", "image", "inspect", nome, "--format", "{{.Id}}"], 30)
        if r.returncode == 0:
            return nome
    return None


class Ffmpeg:
    """ffmpeg do sistema (nativo) ou de uma imagem Docker."""

    def __init__(self, tipo, pasta=None, imagem=None, cpus=None, gpu=False):
        self.tipo, self.imagem, self.cpus, self.gpu = tipo, imagem, cpus, gpu
        exe = ".exe" if WINDOWS else ""
        self.caminho = str(Path(pasta) / f"ffmpeg{exe}") if pasta else (shutil.which("ffmpeg") or "ffmpeg")

    def comando(self, args, pasta_videos=None):
        if self.tipo == "nativo":
            return [self.caminho, "-hide_banner", "-nostdin", *args]
        cmd = ["docker", "run", "--rm", "--entrypoint", "ffmpeg"]
        if self.cpus:
            cmd += ["--cpus", str(self.cpus)]
        if self.gpu:
            cmd += ["--device", "/dev/dri:/dev/dri"]
            gid = grupo_render()
            if gid:
                cmd += ["--group-add", gid]
        if pasta_videos:
            cmd += ["-v", f"{pasta_videos}:/videos"]
        return [*cmd, self.imagem, "-hide_banner", "-nostdin", *args]

    def executar(self, args, pasta_videos=None, timeout=300):
        return executar(self.comando(args, pasta_videos), timeout)

    def encoders(self):
        r = self.executar(["-encoders"], timeout=60)
        return set(re.findall(r"^\s*V\S*\s+(\S+)", r.stdout, re.M)) if r.returncode == 0 else set()


def grupo_render():
    for dispositivo in ("/dev/dri/renderD128", "/dev/dri/card0"):
        if Path(dispositivo).exists():
            try:
                return str(os.stat(dispositivo).st_gid)
            except OSError:
                return None
    return None


# ---------------------------------------------------------------- argumentos de ffmpeg (como o servidor)

def args_encoder(perfil, entrada, duracao):
    """Argumentos do teste para um perfil: `(argumentos, rotulo_do_encoder)`."""
    comum = ["-benchmark", "-t", str(duracao)]
    saida = ["-f", "null", "-"]
    if perfil["encoder"] == "libx264":
        return [*comum, "-i", entrada, "-vf", "scale=1280:720", "-c:v", "libx264", "-preset", "veryfast",
                "-pix_fmt", "yuv420p", "-crf", "23", "-maxrate", BITRATE, "-bufsize", "8000000", "-an",
                "-x264-params", "subme=0:me_range=16:rc_lookahead=10:me=hex:open_gop=0", *saida]
    if perfil["encoder"] == "h264_vaapi":
        disp = perfil.get("dispositivo", "/dev/dri/renderD128")
        return [*comum, "-init_hw_device", f"vaapi=hw:{disp}", "-filter_hw_device", "hw", "-hwaccel", "vaapi",
                "-hwaccel_output_format", "vaapi", "-hwaccel_device", "hw", "-i", entrada, "-vf",
                "scale_vaapi=w=1280:h=720:format=nv12", "-c:v", "h264_vaapi", "-rc_mode", "VBR", "-b:v", BITRATE,
                "-maxrate", BITRATE, "-bufsize", "8000000", "-an", *saida]
    if perfil["encoder"] == "h264_qsv":
        if WINDOWS:
            init = ["-init_hw_device", "d3d11va=d3d11:,vendor_id=0x8086", "-init_hw_device", "qsv=hw@d3d11"]
        else:
            init = ["-init_hw_device", "vaapi=va:/dev/dri/renderD128,driver=iHD", "-init_hw_device", "qsv=hw@va"]
        return [*comum, *init, "-filter_hw_device", "hw", "-hwaccel", "qsv", "-hwaccel_output_format", "qsv", "-i", entrada,
                "-vf", "vpp_qsv=w=1280:h=720:format=nv12", "-c:v", "h264_qsv", "-preset", "veryfast", "-b:v", BITRATE,
                "-maxrate", str(int(BITRATE) + 1), "-rc_init_occupancy", BITRATE, "-bufsize", "8000000", "-an", *saida]
    raise ValueError(perfil["encoder"])


def ler_benchmark(stderr):
    m = re.findall(r"bench: utime=([\d.]+)s stime=([\d.]+)s rtime=([\d.]+)s", stderr)
    if not m:
        return None
    u, s, r = (float(x) for x in m[-1])
    return {"cpu_s": u + s, "tempo_s": r}


def gerar_video(ff, pasta, duracao):
    """Vídeo de teste H.264 8 bits em 1080p (o formato que as GPUs decodificam), gerado uma vez e reaproveitado."""
    destino = pasta / "teste-1080p.mp4"
    if destino.exists():
        return destino
    args = ["-y", "-f", "lavfi", "-i", f"testsrc2=size=1920x1080:rate=30:duration={duracao}", "-vf",
            "noise=alls=12:allf=t,format=yuv420p", "-c:v", "libx264", "-preset", "veryfast", "-crf", "18",
            "-pix_fmt", "yuv420p", "-g", "60", "/videos/teste-1080p.mp4" if ff.tipo == "docker" else str(destino)]
    r = ff.executar(args, pasta_videos=str(pasta), timeout=300)
    return destino if r.returncode == 0 and destino.exists() else None


# ---------------------------------------------------------------- medição

def medir(ff, perfil, pasta, duracao, repeticoes, nucleos):
    entrada = "/videos/teste-1080p.mp4" if ff.tipo == "docker" else str(pasta / "teste-1080p.mp4")
    sonda = ff.executar(args_encoder(perfil, entrada, 2), pasta_videos=str(pasta), timeout=120)
    if sonda.returncode != 0:
        return {"disponivel": False, "motivo": "o encoder não funcionou aqui: " + cauda(sonda.stderr, 160)}
    execucoes = []
    for i in range(repeticoes + 1):  # a primeira rodada aquece caches e é descartada
        t0 = time.perf_counter()
        r = ff.executar(args_encoder(perfil, entrada, duracao), pasta_videos=str(pasta), timeout=600)
        parede = time.perf_counter() - t0
        b = ler_benchmark(r.stderr)
        if r.returncode != 0 or not b:
            return {"disponivel": False, "motivo": "falhou durante a medição: " + cauda(r.stderr, 160)}
        if i:
            execucoes.append({"x_tempo_real": duracao / b["tempo_s"], "nucleos_ocupados": b["cpu_s"] / b["tempo_s"],
                              "tempo_s": b["tempo_s"], "parede_s": parede})
    med = lambda k: statistics.median(e[k] for e in execucoes)
    x, nuc = med("x_tempo_real"), med("nucleos_ocupados")
    return {
        "disponivel": True,
        "x_tempo_real": round(x, 2),
        "nucleos_ocupados": round(nuc, 2),
        "cpu_pct_do_sistema": round(nuc / nucleos * 100, 1),
        "x_tempo_real_por_nucleo": round(x / nuc, 2) if nuc else None,
        "sessoes_simultaneas_estimadas": int(x),
        "repeticoes": [round(e["x_tempo_real"], 2) for e in execucoes],
        "variacao_pct": round((max(e["x_tempo_real"] for e in execucoes) - min(e["x_tempo_real"] for e in execucoes)) / x * 100, 1),
    }


# ---------------------------------------------------------------- recomendação

COMANDOS = {
    "docker-cpu": "docker compose up -d --build   (nenhuma configuração extra)",
    "docker-gpu": "sh tools/gpu-setup.sh && docker compose up -d   (Linux; liga o repasse de /dev/dri)",
    "nativo-cpu": "RUSTYFIN_FFMPEG=<caminho do ffmpeg> RUSTYFIN_VIDEO_ENCODER=libx264 ./rustyfin   (ou cargo run --release)",
    "nativo-gpu": "RUSTYFIN_FFMPEG=<caminho do ffmpeg com o encoder> RUSTYFIN_VIDEO_ENCODER=auto ./rustyfin   (ou cargo run --release)",
}
COMANDOS_WINDOWS = {
    "nativo-cpu": '$env:RUSTYFIN_FFMPEG="<pasta>\ffmpeg.exe"; $env:RUSTYFIN_VIDEO_ENCODER="libx264"; cargo run --release',
    "nativo-gpu": '$env:RUSTYFIN_FFMPEG="<pasta>\ffmpeg.exe"; $env:RUSTYFIN_VIDEO_ENCODER="auto"; cargo run --release   (veja docs/reproducao.md#windows)',
}


def recomendar(resultados):
    ok = {k: v for k, v in resultados.items() if v.get("disponivel")}
    if not ok:
        return {"perfil": None, "motivo": "nenhum perfil pôde ser medido; veja o motivo de cada um em `perfis`."}
    cpu = {k: v for k, v in ok.items() if k.endswith("cpu")}
    gpu = {k: v for k, v in ok.items() if k.endswith("gpu")}
    melhor_cpu = max(cpu, key=lambda k: cpu[k]["x_tempo_real"]) if cpu else None
    melhor_gpu = max(gpu, key=lambda k: gpu[k]["x_tempo_real"]) if gpu else None
    # Entre perfis de processador, o Docker ganha em praticidade quando perde menos de 10% de velocidade.
    if melhor_cpu and "docker-cpu" in cpu and "nativo-cpu" in cpu and cpu["docker-cpu"]["x_tempo_real"] >= 0.9 * cpu["nativo-cpu"]["x_tempo_real"]:
        melhor_cpu = "docker-cpu"
    escolha, motivo = melhor_cpu, ""
    if melhor_gpu and melhor_cpu:
        g, c = gpu[melhor_gpu], cpu[melhor_cpu]
        mais_rapida = g["x_tempo_real"] >= 1.1 * c["x_tempo_real"]
        mais_leve = g["x_tempo_real"] >= 0.85 * c["x_tempo_real"] and g["nucleos_ocupados"] <= 0.5 * c["nucleos_ocupados"]
        if mais_rapida or mais_leve:
            escolha = melhor_gpu
            motivo = (f"a GPU transcodifica {g['x_tempo_real']}× o tempo real usando {g['nucleos_ocupados']} núcleos, contra "
                      f"{c['x_tempo_real']}× e {c['nucleos_ocupados']} núcleos do processador ({melhor_cpu}).")
        else:
            motivo = (f"a GPU ({g['x_tempo_real']}×) não compensa frente ao processador ({c['x_tempo_real']}×, {melhor_cpu}).")
    elif melhor_gpu and not melhor_cpu:
        escolha = melhor_gpu
        motivo = "só o perfil com GPU pôde ser medido."
    if not motivo:
        motivo = f"{escolha} foi o mais rápido entre os disponíveis ({ok[escolha]['x_tempo_real']}× o tempo real)."
    if escolha == "docker-cpu" and "nativo-cpu" in cpu:
        motivo += (f" O Docker ficou a {round(cpu['docker-cpu']['x_tempo_real'] / cpu['nativo-cpu']['x_tempo_real'] * 100)}% do nativo, "
                   "dentro do que vale a praticidade.")
    return {"perfil": escolha, "motivo": motivo, "como_aplicar": (COMANDOS_WINDOWS if WINDOWS else COMANDOS).get(escolha) or COMANDOS.get(escolha),
            "alternativa_sem_gpu": melhor_cpu if escolha.endswith("gpu") else None}


# ---------------------------------------------------------------- principal

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ffmpeg-dir", help="pasta com ffmpeg(.exe) para os perfis nativos (padrão: o do PATH)")
    ap.add_argument("--image", help="imagem Docker do Rustyfin (padrão: rustyfin-rustyfin:latest ou rustyfin:latest)")
    ap.add_argument("--docker-cpus", type=float, help="limita as CPUs do container (como um NAS), por exemplo 2")
    ap.add_argument("--duracao", type=int, default=30, help="segundos de vídeo de teste (padrão 30)")
    ap.add_argument("--repeticoes", type=int, default=3)
    ap.add_argument("--sem-docker", action="store_true")
    ap.add_argument("--sem-nativo", action="store_true")
    ap.add_argument("--json", action="store_true", help="imprime só o JSON (para uma LLM ou um script)")
    ap.add_argument("--out", help="grava o JSON neste arquivo")
    a = ap.parse_args()
    say = (lambda *x: None) if a.json else (lambda *x: print(*x, file=sys.stderr if False else sys.stdout, flush=True))

    docker = {"disponivel": False, "motivo": "ignorado (--sem-docker)"} if a.sem_docker else info_docker()
    ambiente = {
        "script": VERSAO_SCRIPT, "sistema": platform.platform(), "so": "windows" if WINDOWS else platform.system().lower(),
        "cpu": nome_cpu(), "nucleos": os.cpu_count(), "memoria_gib": memoria_gib(), "placas_de_video": placas_de_video(),
        "docker": docker, "duracao_teste_s": a.duracao,
    }
    say(f"Máquina: {ambiente['cpu']} ({ambiente['nucleos']} núcleos), placas: {', '.join(ambiente['placas_de_video']) or 'não detectadas'}")

    pasta = Path(tempfile.gettempdir()) / "rustyfin-perfil"
    pasta.mkdir(exist_ok=True)
    resultados, notas = {}, []

    def tentar(nome, ff, perfil, nucleos):
        say(f"  medindo {nome}...")
        resultados[nome] = medir(ff, perfil, pasta, a.duracao, a.repeticoes, nucleos)
        r = resultados[nome]
        say("    " + (f"{r['x_tempo_real']}× o tempo real, {r['nucleos_ocupados']} núcleos ocupados" if r["disponivel"] else "indisponível: " + r["motivo"]))

    # Quem gera o vídeo de teste: o ffmpeg do Docker (se houver) ou o nativo.
    gerador = None
    imagem = achar_imagem(a.image) if docker.get("disponivel") else None
    nativo = Ffmpeg("nativo", a.ffmpeg_dir)
    nativo_ok = (not a.sem_nativo) and executar([nativo.caminho, "-version"], 15).returncode == 0
    if docker.get("disponivel") and not imagem:
        notas.append("Imagem do Rustyfin não encontrada: rode `docker compose build` (ou use --image).")
    gerador = Ffmpeg("docker", imagem=imagem) if imagem else (nativo if nativo_ok else None)
    if not gerador:
        resultados["erro"] = {"disponivel": False, "motivo": "sem ffmpeg: use o Docker (com a imagem do Rustyfin) ou --ffmpeg-dir"}
        say("Sem ffmpeg para gerar o vídeo de teste.")
    elif not gerar_video(gerador, pasta, a.duracao):
        resultados["erro"] = {"disponivel": False, "motivo": "não consegui gerar o vídeo de teste (o ffmpeg não tem libx264?)"}
    else:
        if imagem:
            nuc = a.docker_cpus or docker.get("cpus") or os.cpu_count()
            tentar("docker-cpu", Ffmpeg("docker", imagem=imagem, cpus=a.docker_cpus), {"encoder": "libx264"}, nuc)
            if WINDOWS or platform.system() == "Darwin":
                resultados["docker-gpu"] = {"disponivel": False, "motivo": "o Docker Desktop não repassa a GPU Intel ao container (Windows e macOS)"}
            elif grupo_render() is None:
                resultados["docker-gpu"] = {"disponivel": False, "motivo": "sem /dev/dri no sistema (não há GPU de renderização)"}
            else:
                tentar("docker-gpu", Ffmpeg("docker", imagem=imagem, cpus=a.docker_cpus, gpu=True), {"encoder": "h264_vaapi"}, nuc)
        elif docker.get("disponivel"):
            resultados["docker-cpu"] = resultados["docker-gpu"] = {"disponivel": False, "motivo": "imagem do Rustyfin não encontrada"}
        if nativo_ok:
            enc = nativo.encoders()
            tentar("nativo-cpu", nativo, {"encoder": "libx264"}, os.cpu_count())
            hw = "h264_qsv" if (WINDOWS and "h264_qsv" in enc) else ("h264_vaapi" if (not WINDOWS and "h264_vaapi" in enc) else None)
            if hw:
                tentar("nativo-gpu", nativo, {"encoder": hw}, os.cpu_count())
            else:
                resultados["nativo-gpu"] = {"disponivel": False, "motivo": "este ffmpeg não tem o encoder de GPU (h264_qsv no Windows, h264_vaapi no Linux)"}
        elif not a.sem_nativo:
            for nome in ("nativo-cpu", "nativo-gpu"):
                resultados[nome] = {"disponivel": False, "motivo": "ffmpeg nativo não encontrado (use --ffmpeg-dir)"}

    saida = {"ambiente": ambiente, "perfis": resultados, "recomendacao": recomendar(resultados), "notas": notas}
    if a.out:
        Path(a.out).write_text(json.dumps(saida, indent=2, ensure_ascii=False), encoding="utf-8")
    if a.json:
        print(json.dumps(saida, indent=2, ensure_ascii=False))
        return
    say("\nResultado (30 s de 1080p reduzidos para 720p a 4 Mbps):")
    say(f"  {'perfil':12s} {'velocidade':>12s} {'núcleos':>9s} {'CPU do sistema':>15s}  situação")
    for nome in ("docker-cpu", "docker-gpu", "nativo-cpu", "nativo-gpu"):
        r = resultados.get(nome)
        if not r:
            continue
        if r["disponivel"]:
            say(f"  {nome:12s} {str(r['x_tempo_real']) + '×':>12s} {r['nucleos_ocupados']:>9} {str(r['cpu_pct_do_sistema']) + '%':>15s}  ok (variação {r['variacao_pct']}%)")
        else:
            say(f"  {nome:12s} {'-':>12s} {'-':>9s} {'-':>15s}  {r['motivo']}")
    rec = saida["recomendacao"]
    say(f"\nRecomendação: {rec['perfil']}\n  {rec['motivo']}")
    if rec.get("como_aplicar"):
        say(f"  Como aplicar: {rec['como_aplicar']}")
    for n in notas:
        say("  Nota: " + n)


if __name__ == "__main__":
    main()
