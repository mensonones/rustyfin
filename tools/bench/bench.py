#!/usr/bin/env python3
"""Comparativo de desempenho Rustyfin x Jellyfin (metodologia em docs/desempenho.md).

Cada sistema roda em um container Docker com os mesmos limites de CPU/memória, um de cada vez, com a mesma
biblioteca montada somente leitura. Só usa a biblioteca padrão do Python e o CLI do Docker.

Exemplo:
    python tools/bench/bench.py --profile folgado --media /caminho/da/biblioteca --out resultados.json
"""
import argparse
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

AUTH = 'MediaBrowser Client="bench", Device="bench", DeviceId="bench1", Version="1"'
PROFILES = {"folgado": {"cpus": "4", "memory": "4g"}, "restrito": {"cpus": "1", "memory": "512m"}}
JF_PASS = "bench123"
RUSTYFIN_IMAGE = "rustyfin-rustyfin:latest"  # fixe uma versão com --rustyfin-image (a imagem :latest muda a cada build)
VIDEO_EXT = (".mp4", ".mkv", ".avi", ".mov", ".webm", ".m4v", ".ts")


def log(msg):
    print(time.strftime("%H:%M:%S"), msg, flush=True)


def docker(*args, check=True, timeout=900):
    r = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if check and r.returncode != 0:
        raise RuntimeError(f"docker {' '.join(args[:3])}: {r.stderr.strip()[:300]}")
    return r.stdout.strip()


def pct(values, p):
    v = sorted(values)
    return v[min(len(v) - 1, int(len(v) * p))] if v else None


def med(values):
    return statistics.median(values) if values else None


class Http:
    def __init__(self, base):
        self.base, self.token, self.uid = base, None, None

    def req(self, path, method="GET", body=None, raw=False, headers=None, timeout=120):
        h = {"Authorization": AUTH + (f', Token="{self.token}"' if self.token else "")}
        h.update(headers or {})
        data = None
        if body is not None:
            h["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        r = urllib.request.Request(self.base + path, data=data, method=method, headers=h)
        t0 = time.perf_counter()
        try:
            with urllib.request.urlopen(r, timeout=timeout) as x:
                d = x.read()
                code = x.status
        except urllib.error.HTTPError as e:
            d, code = e.read(), e.code
        except Exception:
            d, code = b"", 0
        ms = (time.perf_counter() - t0) * 1000
        if raw:
            return code, d, ms
        try:
            return code, (json.loads(d) if d else None), ms
        except ValueError:
            return code, None, ms

    def u(self, path):
        return path + ("&" if "?" in path else "?") + "UserId=" + (self.uid or "")


class Sampler(threading.Thread):
    """Amostra `docker stats` (memória e CPU) de um container a cada segundo."""

    def __init__(self, name):
        super().__init__(daemon=True)
        self.name, self.samples, self._halt = name, [], threading.Event()

    def run(self):
        while not self._halt.is_set():
            try:
                out = docker("stats", "--no-stream", "--format", "{{.MemUsage}}|{{.CPUPerc}}", self.name, check=False, timeout=20)
                mem, cpu = out.split("|")
                used = mem.split("/")[0].strip()
                num = float("".join(c for c in used if c.isdigit() or c == "."))
                mib = num * {"KiB": 1 / 1024, "MiB": 1, "GiB": 1024, "kB": 1 / 1024, "MB": 1, "GB": 1024}.get(used.lstrip("0123456789."), 1)
                self.samples.append((time.time(), mib, float(cpu.strip("%"))))
            except Exception:
                pass
            self._halt.wait(0.2)

    def finish(self):
        self._halt.set()
        self.join(timeout=10)
        mem = [s[1] for s in self.samples]
        cpu = [s[2] for s in self.samples]
        return {"mem_mib_med": med(mem), "mem_mib_max": max(mem) if mem else None, "cpu_pct_med": med(cpu), "cpu_pct_max": max(cpu) if cpu else None, "n": len(mem)}


def wait_quiet(name, limit=240):
    """Espera o container ficar ocioso (CPU < 3% em 3 amostras seguidas): termina tarefas em segundo plano."""
    calm, end = 0, time.time() + limit
    while time.time() < end:
        out = docker("stats", "--no-stream", "--format", "{{.CPUPerc}}", name, check=False, timeout=20)
        try:
            calm = calm + 1 if float(out.strip("%")) < 3 else 0
        except ValueError:
            calm = 0
        if calm >= 3:
            return True
        time.sleep(1)
    return False


class Server:
    def __init__(self, kind, port, profile, media_mounts):
        self.kind, self.port, self.profile, self.media = kind, port, PROFILES[profile], media_mounts
        self.name = f"bench-{kind}"
        self.http = Http(f"http://127.0.0.1:{port}")

    # ---- ciclo de vida
    def volumes(self):
        return [f"bench_{self.kind}_data"] if self.kind == "rustyfin" else ["bench_jf_cfg", "bench_jf_cache"]

    def start(self, fresh=True):
        docker("rm", "-f", self.name, check=False)
        if fresh:
            for v in self.volumes():
                docker("volume", "rm", "-f", v, check=False)
        args = ["run", "-d", "--name", self.name, "-p", f"{self.port}:8096", "--cpus", self.profile["cpus"], "--memory", self.profile["memory"]]
        for host, cont in self.media.items():
            args += ["-v", f"{host}:{cont}:ro"]
        if self.kind == "rustyfin":
            args += ["-v", "bench_rustyfin_data:/data", "-e", "RUSTYFIN_MEDIA_DIRS=/nonexistent", "-e", "RUSTYFIN_CACHE_DIR=/data/cache",
                     "-e", "RUSTYFIN_DISCOVERY=false", "-e", "RUSTYFIN_WATCH=false", "-e", "RUSTYFIN_SCAN_INTERVAL_SECS=0",
                     "-e", "RUSTYFIN_MAX_TRANSCODES=8", RUSTYFIN_IMAGE]
        else:
            args += ["-v", "bench_jf_cfg:/config", "-v", "bench_jf_cache:/cache", "jellyfin/jellyfin:latest"]
        t0 = time.perf_counter()
        docker(*args)
        first = self.wait_http(240)
        return {"first_200_s": first - t0}

    def wait_http(self, limit):
        end = time.time() + limit
        while time.time() < end:
            if self.http.req("/System/Info/Public", timeout=2)[0] == 200:
                return time.perf_counter()
            time.sleep(0.05)
        raise RuntimeError("servidor não respondeu")

    def restart_time(self):
        t0 = time.perf_counter()
        docker("restart", self.name)
        return self.wait_http(240) - t0

    def stop(self):
        docker("rm", "-f", self.name, check=False)

    # ---- preparação
    def setup(self):
        if self.kind == "jellyfin":
            for _ in range(120):  # o wizard só aceita pedidos depois que o servidor termina de subir
                if self.http.req("/Startup/Configuration")[0] == 200:
                    break
                time.sleep(0.5)
            self.http.req("/Startup/Configuration", "POST", {"UICulture": "pt-BR", "MetadataCountryCode": "BR", "PreferredMetadataLanguage": "pt"})
            self.http.req("/Startup/User")
            self.http.req("/Startup/User", "POST", {"Name": "Admin", "Password": JF_PASS})
            self.http.req("/Startup/RemoteAccess", "POST", {"EnableRemoteAccess": True, "EnableAutomaticPortMapping": False})
            self.http.req("/Startup/Complete", "POST")
        pw = JF_PASS if self.kind == "jellyfin" else ""
        c, v, _ = self.http.req("/Users/AuthenticateByName", "POST", {"Username": "Admin", "Pw": pw})
        if c != 200:
            raise RuntimeError(f"login falhou ({c})")
        self.http.token, self.http.uid = v["AccessToken"], v["User"]["Id"]

    def add_libraries(self, libs):
        """libs: lista de (nome, tipo 'movies'|'tvshows', caminho no container)."""
        if self.kind == "rustyfin":
            for lib in self.http.req("/admin/api/folders")[1]:
                for p in lib["Locations"]:
                    self.http.req("/admin/api/folders/paths?" + urllib.parse.urlencode({"name": lib["Name"], "path": p}), "DELETE")
            t0 = time.perf_counter()
            for name, _, path in libs:
                self.http.req("/admin/api/folders/paths", "POST", {"Name": "Filmes" if _ == "movies" else "Séries", "Path": path})
            return t0
        for name, ctype, path in libs:
            opts = {"LibraryOptions": {"PathInfos": [{"Path": path}], "EnableInternetProviders": False, "MetadataSavers": [],
                                      "EnableRealtimeMonitor": False, "ExtractChapterImagesDuringLibraryScan": False,
                                      "EnableTrickplayImageExtraction": False, "ExtractTrickplayImagesDuringLibraryScan": False,
                                      "TypeOptions": [], "AutomaticRefreshIntervalDays": 0}}
            self.http.req(f"/Library/VirtualFolders?name={urllib.parse.quote(name)}&collectionType={ctype}&refreshLibrary=false", "POST", opts)
        t0 = time.perf_counter()
        self.http.req("/Library/Refresh", "POST")
        return t0

    def count(self):
        c, v, _ = self.http.req(self.http.u("/Items?Recursive=true&IncludeItemTypes=Movie,Episode&Limit=1"))
        return v["TotalRecordCount"] if c == 200 and v else -1

    def scan_busy(self):
        if self.kind == "rustyfin":
            c, v, _ = self.http.req("/admin/api/status")
            return bool(v and (v["scan"]["running"] or v["scan"]["pending"]))
        c, v, _ = self.http.req("/ScheduledTasks")
        t = next((x for x in (v or []) if x["Key"] == "RefreshLibrary"), None)
        return bool(t and t["State"] != "Idle")

    def wait_count(self, expected, t0, limit):
        while time.perf_counter() - t0 < limit:
            if self.count() >= expected:
                return time.perf_counter() - t0
            time.sleep(0.1)
        return None

    def wait_idle(self, t0, limit):
        seen_busy = False
        while time.perf_counter() - t0 < limit:
            busy = self.scan_busy()
            seen_busy |= busy
            if not busy and (seen_busy or time.perf_counter() - t0 > 3):
                return time.perf_counter() - t0
            time.sleep(0.1)
        return None

    def refresh(self):
        self.http.req("/Library/Refresh", "POST")

    # ---- consultas
    def find_item(self, term):
        c, v, _ = self.http.req(self.http.u("/Items?Recursive=true&IncludeItemTypes=Movie,Episode&Fields=MediaSources&SearchTerm=" + urllib.parse.quote(term)))
        items = (v or {}).get("Items", [])
        return items[0]["Id"] if items else None

    def biggest_item(self):
        c, v, _ = self.http.req(self.http.u("/Items?Recursive=true&IncludeItemTypes=Movie,Episode&Fields=MediaSources&Limit=200"))
        items = (v or {}).get("Items", [])
        items.sort(key=lambda i: (i.get("MediaSources") or [{}])[0].get("Size", 0), reverse=True)
        return items[0]["Id"] if items else None

    def stream_url(self, item):
        return f"/Videos/{item}/stream?static=true&mediaSourceId={item}&api_key={self.http.token}"


# ======================================================================== cenários
def load_latency(srv, name, path, n, conc):
    times, errs, lock = [], [0], threading.Lock()

    def worker(k):
        for _ in range(k):
            code, _, ms = srv.http.req(path, raw=True)
            with lock:
                times.append(ms)
                errs[0] += code >= 400 or code == 0

    t0 = time.perf_counter()
    ths = [threading.Thread(target=worker, args=(max(1, n // conc),)) for _ in range(conc)]
    [t.start() for t in ths]
    [t.join() for t in ths]
    wall = time.perf_counter() - t0
    return {"rps": len(times) / wall, "p50": med(times), "p95": pct(times, 0.95), "p99": pct(times, 0.99), "err": errs[0]}


def repeat(fn, reps=4):
    runs = [fn() for _ in range(reps)][1:]  # descarta a 1ª
    keys = runs[0].keys()
    return {k: med([r[k] for r in runs]) for k in keys}


def scenario_api(srv, item, series, reps):
    out = {}
    paths = {
        "UserViews": srv.http.u("/UserViews"),
        "Items?Recursive": srv.http.u("/Items?Recursive=true"),
        "Items/{id}": srv.http.u(f"/Items/{item}"),
        "Latest": srv.http.u("/Items/Latest?Limit=20"),
    }
    if series:
        paths["Shows/Seasons"] = srv.http.u(f"/Shows/{series}/Seasons")
    for conc in (1, 10, 50):
        for name, path in paths.items():
            out[f"{name} c={conc}"] = repeat(lambda: load_latency(srv, name, path, 300, conc), reps)
    return out


def read_stream(srv, url, nbytes, rng=None):
    h = {"Range": rng} if rng else {}
    t0 = time.perf_counter()
    r = urllib.request.Request(srv.http.base + url, headers=h)
    with urllib.request.urlopen(r, timeout=120) as x:
        x.read(1)
        ttfb = (time.perf_counter() - t0) * 1000
        got = 1
        while got < nbytes:
            d = x.read(1 << 20)
            if not d:
                break
            got += len(d)
    dt = time.perf_counter() - t0
    return ttfb, got / 1e6 / dt


def scenario_direct(srv, item, size):
    url = srv.stream_url(item)
    out = {}
    runs = [read_stream(srv, url, 300 << 20) for _ in range(4)][1:]
    out["ttfb_ms"] = med([r[0] for r in runs])
    out["mb_s_1stream"] = med([r[1] for r in runs])
    seeks = []
    for _ in range(200):
        off = random.randrange(0, max(1, size - 70000))
        t = time.perf_counter()
        try:
            r = urllib.request.Request(srv.http.base + url, headers={"Range": f"bytes={off}-{off + 65535}"})
            urllib.request.urlopen(r, timeout=30).read()
            seeks.append((time.perf_counter() - t) * 1000)
        except Exception:
            pass
    out["seek_p50_ms"], out["seek_p95_ms"] = med(seeks), pct(seeks, 0.95)
    for n in (5, 10, 20):
        aggs, oks = [], []
        for _ in range(4):
            res, lock = [], threading.Lock()

            def w():
                try:
                    res.append(read_stream(srv, url, 100 << 20)[1])
                except Exception:
                    pass

            ths = [threading.Thread(target=w) for _ in range(n)]
            [t.start() for t in ths]
            [t.join() for t in ths]
            aggs.append(sum(res))
            oks.append(len(res))
        out[f"mb_s_{n}streams_agg"] = med(aggs[1:])
        out[f"mb_s_{n}streams_ok"] = med(oks[1:])
    return out


# Limite de bitrate abaixo do bitrate do arquivo: os dois precisam recodificar o vídeo (senão o Jellyfin só copia o
# vídeo H.264 para o HLS, o que não é o mesmo trabalho).
FORCE_BITRATE = 700000
PROFILE_HLS = {"MaxStreamingBitrate": FORCE_BITRATE,
               "DirectPlayProfiles": [{"Type": "Video", "Container": "webm", "VideoCodec": "vp9", "AudioCodec": "opus"}],
               "TranscodingProfiles": [{"Type": "Video", "Container": "ts", "Protocol": "hls", "VideoCodec": "h264", "AudioCodec": "aac"}]}


def hls_session(srv, item, nseg, strip_abr=False):
    """Pede uma sessão HLS transcodificada e baixa `nseg` segmentos em sequência."""
    t0 = time.perf_counter()
    body = {"DeviceProfile": PROFILE_HLS, "EnableDirectPlay": False, "EnableDirectStream": False, "EnableTranscoding": True,
            "UserId": srv.http.uid, "MaxStreamingBitrate": FORCE_BITRATE}
    c, v, _ = srv.http.req(f"/Items/{item}/PlaybackInfo?UserId={srv.http.uid}&MediaSourceId={item}", "POST", body)
    ms = (v or {}).get("MediaSources", [{}])[0]
    url = ms.get("TranscodingUrl")
    if not url:
        raise RuntimeError(f"sem TranscodingUrl (código {c})")
    if strip_abr:  # uma única qualidade, como o Jellyfin, para comparar o mesmo trabalho do ffmpeg
        url = url.replace("&abr=true", "").replace("abr=true&", "")
    sep = "&" if "?" in url else "?"
    code, master, _ = srv.http.req(url + f"{sep}api_key={srv.http.token}", raw=True)
    lines = [l for l in master.decode().splitlines() if l and not l.startswith("#")]
    base = url.split("?")[0].rsplit("/", 1)[0]
    pl_url = lines[0] if lines[0].startswith("/") else base + "/" + lines[0]
    code, pl, _ = srv.http.req(pl_url + ("&" if "?" in pl_url else "?") + f"api_key={srv.http.token}", raw=True)
    segs = [l for l in pl.decode().splitlines() if l and not l.startswith("#")]
    if not segs:
        raise RuntimeError("playlist sem segmentos")
    first = None
    for i, s in enumerate(segs[:nseg]):
        su = s if s.startswith("/") else base + "/" + s
        code, d, _ = srv.http.req(su + ("&" if "?" in su else "?") + f"api_key={srv.http.token}", raw=True, timeout=120)
        if code != 200 or len(d) < 1000:
            raise RuntimeError(f"segmento {i} falhou ({code})")
        if first is None:
            first = time.perf_counter() - t0
    total = time.perf_counter() - t0
    return {"first_seg_s": first, "total_s": total, "x_realtime": nseg * 6 / total}


def scenario_transcode(srv, item, sessions=(1, 2, 4), nseg=6, reps=3, strip_abr=False):
    out = {}
    for k in sessions:
        runs = []
        for _ in range(reps):
            smp = Sampler(srv.name)
            smp.start()
            res, errs, lock = [], [], threading.Lock()

            def w():
                try:
                    r = hls_session(srv, item, nseg, strip_abr)
                    with lock:
                        res.append(r)
                except Exception as e:
                    with lock:
                        errs.append(str(e)[:80])

            ths = [threading.Thread(target=w) for _ in range(k)]
            [t.start() for t in ths]
            [t.join() for t in ths]
            st = smp.finish()
            runs.append({"ok": len(res), "err": errs[:1], "first_seg_s": med([r["first_seg_s"] for r in res]),
                         "x_realtime_each": med([r["x_realtime"] for r in res]), "cpu_pct_max": st["cpu_pct_max"], "mem_mib_max": st["mem_mib_max"]})
            time.sleep(3)
        good = [r for r in runs if r["ok"]]
        out[f"{k}_sessoes"] = {
            "ok_de": f"{med([r['ok'] for r in runs])}/{k}",
            "first_seg_s": med([r["first_seg_s"] for r in good]),
            "x_realtime_each": med([r["x_realtime_each"] for r in good]),
            "cpu_pct_max": med([r["cpu_pct_max"] for r in runs]),
            "mem_mib_max": med([r["mem_mib_max"] for r in runs]),
            "erros": next((r["err"] for r in runs if r["err"]), []),
        }
    return out


def idle_mem(name):
    wait_quiet(name, 120)
    s = Sampler(name)
    s.start()
    time.sleep(6)
    return s.finish()["mem_mib_med"]


def scenario_soak(srv, item, minutes):
    mem_before = idle_mem(srv.name)
    stop, errs, total = threading.Event(), [0], [0]
    lock = threading.Lock()
    url = srv.stream_url(item)

    def streamer():
        while not stop.is_set():
            try:
                r = urllib.request.Request(srv.http.base + url, headers={"Range": f"bytes={random.randrange(0, 5 << 20)}-"})
                with urllib.request.urlopen(r, timeout=30) as x:
                    for _ in range(20):
                        if stop.is_set() or not x.read(64 << 10):
                            break
                        time.sleep(0.25)
            except Exception:
                with lock:
                    errs[0] += 1
                stop.wait(1)

    def browser():
        paths = [srv.http.u("/UserViews"), srv.http.u("/Items?Recursive=true&Limit=50"), srv.http.u("/Items/Latest?Limit=20"), srv.http.u(f"/Items/{item}")]
        i = 0
        while not stop.is_set():
            code, _, _ = srv.http.req(paths[i % len(paths)], raw=True)
            with lock:
                total[0] += 1
                errs[0] += code >= 400 or code == 0
            i += 1
            stop.wait(0.2)

    def transcoder():
        while not stop.is_set():
            try:
                hls_session(srv, item, 4)
            except Exception:
                with lock:
                    errs[0] += 1
            stop.wait(5)

    ths = [threading.Thread(target=f, daemon=True) for f in (streamer, streamer, browser, transcoder)]
    smp = Sampler(srv.name)
    smp.start()
    [t.start() for t in ths]
    mem_series = []
    end = time.time() + minutes * 60
    while time.time() < end:
        time.sleep(15)
        if smp.samples:
            mem_series.append(round(smp.samples[-1][1], 1))
    stop.set()
    [t.join(timeout=30) for t in ths]
    st = smp.finish()
    mem_after = idle_mem(srv.name)
    return {"minutos": minutes, "mem_repouso_antes_mib": mem_before, "mem_repouso_depois_mib": mem_after, "mem_mib_inicio": mem_series[0] if mem_series else None, "mem_mib_fim": mem_series[-1] if mem_series else None,
            "mem_mib_max": st["mem_mib_max"], "cpu_pct_med": st["cpu_pct_med"], "requisicoes_api": total[0], "erros": errs[0], "serie_mem_mib": mem_series}


def ensure_base_clip(scale_root):
    os.makedirs(scale_root, exist_ok=True)
    base = os.path.join(scale_root, "base.mp4")
    if not os.path.exists(base):
        docker("run", "--rm", "-v", f"{scale_root}:/w", "--entrypoint", "ffmpeg", RUSTYFIN_IMAGE, "-v", "error", "-y", "-f", "lavfi", "-i",
               "testsrc=duration=2:size=160x120:rate=10", "-f", "lavfi", "-i", "sine=duration=2", "-c:v", "libx264", "-c:a", "aac", "/w/base.mp4")
    return base


def make_scale_dir(path, n, base):
    """Cria `n` clipes minúsculos (cópias de um mesmo arquivo) para medir a varredura em escala."""
    os.makedirs(path, exist_ok=True)
    for i in range(1, n + 1):
        dst = os.path.join(path, f"Clipe {i:05d} ({1990 + i % 30}).mp4")
        if not os.path.exists(dst):
            shutil.copyfile(base, dst)
    for f in os.listdir(path):  # sobras de execuções anteriores
        if f.endswith("_novo.mp4"):
            os.remove(os.path.join(path, f))


def count_videos(root):
    return sum(1 for _, _, fs in os.walk(root) for f in fs if f.lower().endswith(VIDEO_EXT))


def warm_cache(root, whole="Mandalor"):
    """Aquece o cache de arquivos *dentro da VM do Docker* (onde os containers leem): o começo e o fim de cada vídeo
    (índice que o ffprobe e os players procuram) e o arquivo usado nos testes de streaming inteiro.
    Sem isto, o sistema medido primeiro paga a leitura fria do disco montado do Windows (muito lenta)."""
    script = (
        r'find /m -type f \( -iname "*.mp4" -o -iname "*.mkv" -o -iname "*.avi" -o -iname "*.mov" \) | '
        'while read f; do case "$f" in *' + whole + '*) cat "$f" > /dev/null;; *) head -c 50331648 "$f" > /dev/null; tail -c 50331648 "$f" > /dev/null;; esac; done; '
        'echo ok'
    )
    t0 = time.time()
    out = docker("run", "--rm", "-v", f"{root}:/m:ro", "--entrypoint", "sh", RUSTYFIN_IMAGE, "-c", script, timeout=3600)
    return out.strip(), round(time.time() - t0)


def run_system(kind, args, profile, results):
    port = 18096 if kind == "rustyfin" else 18097
    media = {args.media: "/media"}
    scale_root = os.path.join(args.workdir, "scale")
    for n in args.scale:
        media[os.path.join(scale_root, str(n))] = f"/scale{n}"
    srv = Server(kind, port, profile, media)
    res = results.setdefault(profile, {}).setdefault(kind, {})
    log(f"=== {kind} ({profile}) ===")
    img = {"rustyfin": RUSTYFIN_IMAGE, "jellyfin": "jellyfin/jellyfin:latest"}[kind]
    res["A_imagem_mb"] = round(int(docker("image", "inspect", img, "--format", "{{.Size}}")) / 1e6)

    if not args.skip_warm:
        log("aquecendo o cache (dentro da VM do Docker)...")
        log(f"  cache aquecido: {warm_cache(args.media)}")
    # ---- A: pegada de recursos
    smp0 = Sampler(srv.name)
    res["A_subida"] = srv.start(fresh=True)
    smp0.start()
    srv.setup()
    wait_quiet(srv.name)
    smp0 = Sampler(srv.name)
    smp0.start()
    time.sleep(args.idle)
    res["A_ocioso_vazio"] = smp0.finish()
    res["A_restart_quente_s"] = srv.restart_time()
    srv.setup()

    # ---- B: biblioteca real
    expected = count_videos(args.media)
    smp = Sampler(srv.name)
    smp.start()
    t0 = srv.add_libraries([("Filmes", "movies", "/media/filmes"), ("Séries", "tvshows", "/media/series")])
    visible = srv.wait_count(expected, t0, 600)
    idle = srv.wait_idle(t0, 600)
    st = smp.finish()
    res["B_scan_inicial"] = {"arquivos": expected, "visivel_s": visible, "concluido_s": idle, "mem_mib_max": st["mem_mib_max"], "cpu_pct_max": st["cpu_pct_max"]}
    log(f"scan inicial: visível={visible} concluído={idle}")
    res["B_quieto_apos_scan_s"] = None
    tq = time.perf_counter()
    wait_quiet(srv.name)
    res["B_quieto_apos_scan_s"] = time.perf_counter() - tq
    smp = Sampler(srv.name)
    smp.start()
    time.sleep(args.idle)
    res["A_ocioso_indexado"] = smp.finish()
    t0 = time.perf_counter()
    srv.refresh()
    res["B_rescan_s"] = srv.wait_idle(t0, 300)
    item = srv.find_item("Mandalor") or srv.biggest_item()
    big = srv.biggest_item()
    c, v, _ = srv.http.req(srv.http.u(f"/Items/{item}?Fields=MediaSources"))
    size = ((v or {}).get("MediaSources") or [{}])[0].get("Size", 1 << 30)
    c, sv, _ = srv.http.req(srv.http.u("/Items?Recursive=true&IncludeItemTypes=Series&Limit=1"))
    series = ((sv or {}).get("Items") or [{}])[0].get("Id")

    # ---- C/D/E: API, Direct Play, transcode (com a biblioteca real)
    for key, fn in (("C_api", lambda: scenario_api(srv, item, series, args.reps)),
                    ("D_direct_play", lambda: scenario_direct(srv, item, size)),
                    ("E_transcode", lambda: scenario_transcode(srv, item, args.transcode_sessions, strip_abr=True)),
                    ("E_transcode_abr", lambda: scenario_transcode(srv, item, args.transcode_sessions[:1]) if kind == "rustyfin" else {})):
        if key[0] not in args.scenarios:
            continue
        log(f"cenário {key}")
        smp = Sampler(srv.name)
        smp.start()
        try:
            res[key] = fn()
        except Exception as e:
            res[key] = {"erro": str(e)[:200]}
            log(f"  falhou: {e}")
        res[key + "_recursos"] = smp.finish()

    # ---- F: estabilidade
    if "F" in args.scenarios and args.soak_min > 0:
        log("cenário F (estabilidade)")
        try:
            res["F_soak"] = scenario_soak(srv, item, args.soak_min)
        except Exception as e:
            res["F_soak"] = {"erro": str(e)[:200]}
    # ---- B (escala): um servidor novo (volume novo) por tamanho, para não herdar estado de outros cenários
    if "B" in args.scenarios:
        for n in args.scale:
            log(f"escala {n}")
            try:
                srv.start(fresh=True)
                srv.setup()
                wait_quiet(srv.name)
                smp = Sampler(srv.name)
                smp.start()
                t0 = srv.add_libraries([("Filmes", "movies", f"/scale{n}")])
                visible = srv.wait_count(n, t0, args.scale_timeout)
                idle = srv.wait_idle(t0, args.scale_timeout)
                st = smp.finish()
                entry = {"arquivos": n, "visivel_s": visible, "concluido_s": idle, "mem_mib_max": st["mem_mib_max"], "cpu_pct_max": st["cpu_pct_max"]}
                if visible:
                    wait_quiet(srv.name)
                    lst = [load_latency(srv, "lista", srv.http.u("/Items?Recursive=true&IncludeItemTypes=Movie&Limit=50&SortBy=SortName"), 100, 1) for _ in range(3)]
                    full = [load_latency(srv, "tudo", srv.http.u(f"/Items?Recursive=true&IncludeItemTypes=Movie&Limit={n}"), 6, 1) for _ in range(2)]
                    entry["lista50_p50_ms"], entry["tudo_p50_ms"] = med([r["p50"] for r in lst[1:]]), med([r["p50"] for r in full[1:]])
                if visible and n == args.scale[0]:
                    novo = os.path.join(scale_root, str(n), "Clipe novo (2024)_novo.mp4")
                    shutil.copyfile(os.path.join(scale_root, "base.mp4"), novo)
                    t1 = time.perf_counter()
                    srv.refresh()
                    entry["arquivo_novo_s"] = srv.wait_count(n + 1, t1, 120)
                    os.remove(novo)
                res.setdefault("B_escala", {})[str(n)] = entry
            except Exception as e:
                res.setdefault("B_escala", {})[str(n)] = {"erro": str(e)[:200]}
    srv.stop()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--profile", choices=list(PROFILES), default="folgado")
    ap.add_argument("--systems", default="rustyfin,jellyfin")
    ap.add_argument("--media", required=True, help="pasta da biblioteca real (com filmes/ e series/)")
    ap.add_argument("--workdir", default=os.path.join(tempfile.gettempdir(), "rustyfin-bench"))
    ap.add_argument("--scenarios", default="ABCDEF")
    ap.add_argument("--scale", default="1000,3000", help="tamanhos da biblioteca sintética (vazio = sem escala)")
    ap.add_argument("--scale-timeout", type=int, default=900)
    ap.add_argument("--reps", type=int, default=4)
    ap.add_argument("--idle", type=int, default=20, help="segundos de medição em repouso")
    ap.add_argument("--soak-min", type=int, default=8)
    ap.add_argument("--transcode-sessions", default="1,2,4")
    ap.add_argument("--skip-warm", action="store_true", help="não aquece o cache de arquivos antes de cada sistema")
    ap.add_argument("--rustyfin-image", default="rustyfin-rustyfin:latest", help="imagem do Rustyfin a medir (use uma tag fixa)")
    ap.add_argument("--out", default="resultados.json")
    args = ap.parse_args()
    global RUSTYFIN_IMAGE
    RUSTYFIN_IMAGE = args.rustyfin_image
    args.scale = [int(x) for x in args.scale.split(",") if x]
    args.transcode_sessions = tuple(int(x) for x in args.transcode_sessions.split(","))
    os.makedirs(args.workdir, exist_ok=True)
    base = ensure_base_clip(os.path.join(args.workdir, "scale"))
    for n in args.scale:
        make_scale_dir(os.path.join(args.workdir, "scale", str(n)), n, base)
    results = json.load(open(args.out)) if os.path.exists(args.out) else {}
    results["_meta"] = {"data": time.strftime("%Y-%m-%d %H:%M"), "docker_cpus": docker("info", "--format", "{{.NCPU}}"), "profile": PROFILES}
    for kind in args.systems.split(","):
        try:
            run_system(kind, args, args.profile, results)
        except Exception as e:
            results.setdefault(args.profile, {}).setdefault(kind, {})["erro_geral"] = str(e)[:300]
            log(f"{kind} falhou: {e}")
            docker("rm", "-f", f"bench-{kind}", check=False)
        json.dump(results, open(args.out, "w"), indent=1, ensure_ascii=False)
    log(f"resultados em {args.out}")


if __name__ == "__main__":
    sys.exit(main())
