#!/usr/bin/env python3
"""Gera as tabelas em Markdown de docs/desempenho.md a partir dos JSON de bench.py.

Uso: python tools/bench/report.py tools/bench/results/folgado.json [tools/bench/results/restrito.json]
"""
import json
import sys


def f(v, nd=1, unit=""):
    if v is None:
        return "n/d"
    if isinstance(v, str):
        return v
    return f"{v:,.{nd}f}".replace(",", " ").replace(".", ",") + unit


def ratio(a, b, better="low"):
    if a in (None, 0) or b in (None, 0):
        return ""
    x = b / a if better == "low" else a / b
    return f"{x:.1f}×".replace(".", ",")


def row(name, r, j, key, nd=1, unit="", better="low", sub=None):
    def get(d):
        v = d
        for k in ([sub] if sub else []) + [key]:
            v = (v or {}).get(k) if isinstance(v, dict) else None
        return v
    a, b = get(r), get(j)
    return f"| {name} | {f(a, nd, unit)} | {f(b, nd, unit)} | {ratio(a, b, better)} |"


def section(profile, data):
    r, j = data.get("rustyfin", {}), data.get("jellyfin", {})
    out = [f"### Perfil {profile}", "", "| Métrica | Rustyfin | Jellyfin | Vantagem do Rustyfin |", "|---|---|---|---|"]
    out.append(row("Imagem Docker (MB)", r, j, "A_imagem_mb", 0))
    out.append(row("Subida até responder (s, volume novo)", r.get("A_subida", {}), j.get("A_subida", {}), "first_200_s", 1))
    out.append(f"| Reinício a quente (s) | {f(r.get('A_restart_quente_s'))} | {f(j.get('A_restart_quente_s'))} | {ratio(r.get('A_restart_quente_s'), j.get('A_restart_quente_s'))} |")
    out.append(row("RAM em repouso, biblioteca vazia (MiB)", r.get("A_ocioso_vazio", {}), j.get("A_ocioso_vazio", {}), "mem_mib_med", 0))
    out.append(row("RAM em repouso, biblioteca indexada (MiB)", r.get("A_ocioso_indexado", {}), j.get("A_ocioso_indexado", {}), "mem_mib_med", 0))
    out.append(row("CPU em repouso, indexada (%)", r.get("A_ocioso_indexado", {}), j.get("A_ocioso_indexado", {}), "cpu_pct_med", 1))
    out += ["", "**Biblioteca**", "", "| Métrica | Rustyfin | Jellyfin | Vantagem |", "|---|---|---|---|"]
    out.append(row("1ª varredura de 18 vídeos reais: visíveis na API (s)", r.get("B_scan_inicial", {}), j.get("B_scan_inicial", {}), "visivel_s", 1))
    out.append(row("1ª varredura: concluída (s)", r.get("B_scan_inicial", {}), j.get("B_scan_inicial", {}), "concluido_s", 1))
    out.append(row("Pico de RAM na varredura (MiB)", r.get("B_scan_inicial", {}), j.get("B_scan_inicial", {}), "mem_mib_max", 0))
    out.append(f"| Rescan sem mudanças (s) | {f(r.get('B_rescan_s'), 2)} | {f(j.get('B_rescan_s'), 2)} | {ratio(r.get('B_rescan_s'), j.get('B_rescan_s'))} |")
    for n in sorted({k for d in (r, j) for k in (d.get("B_escala") or {})}, key=int):
        out.append(row(f"Varredura de {n} clipes (s)", (r.get("B_escala") or {}).get(n, {}), (j.get("B_escala") or {}).get(n, {}), "concluido_s", 1))
        out.append(row(f"  pico de RAM com {n} clipes (MiB)", (r.get("B_escala") or {}).get(n, {}), (j.get("B_escala") or {}).get(n, {}), "mem_mib_max", 0))
        out.append(row(f"  listar 50 itens com {n} clipes (ms, p50)", (r.get("B_escala") or {}).get(n, {}), (j.get("B_escala") or {}).get(n, {}), "lista50_p50_ms", 1))
    first = sorted((r.get("B_escala") or {}).keys(), key=int)[:1]
    for n in first:
        out.append(row(f"Arquivo novo aparece (s, com {n} clipes)", (r.get("B_escala") or {}).get(n, {}), (j.get("B_escala") or {}).get(n, {}), "arquivo_novo_s", 1))

    out += ["", "**API (p50 em ms e requisições por segundo; mediana de 3 rodadas)**", "", "| Rota | Conexões | Rustyfin p50 | Jellyfin p50 | Rustyfin req/s | Jellyfin req/s |", "|---|---|---|---|---|---|"]
    ra, ja = r.get("C_api", {}), j.get("C_api", {})
    for key in ra or ja:
        name, _, conc = key.rpartition(" c=")
        a, b = ra.get(key, {}), ja.get(key, {})
        out.append(f"| `{name}` | {conc} | {f(a.get('p50'), 1)} | {f(b.get('p50'), 1)} | {f(a.get('rps'), 0)} | {f(b.get('rps'), 0)} |")

    out += ["", "**Direct Play**", "", "| Métrica | Rustyfin | Jellyfin | Vantagem |", "|---|---|---|---|"]
    rd, jd = r.get("D_direct_play", {}), j.get("D_direct_play", {})
    out.append(row("Tempo até o 1º byte (ms)", rd, jd, "ttfb_ms", 1))
    out.append(row("Vazão de 1 stream (MB/s)", rd, jd, "mb_s_1stream", 0, better="high"))
    out.append(row("Seek de 64 KB, p50 (ms)", rd, jd, "seek_p50_ms", 1))
    out.append(row("Seek de 64 KB, p95 (ms)", rd, jd, "seek_p95_ms", 1))
    for n in (5, 10, 20):
        out.append(row(f"{n} streams simultâneos, vazão agregada (MB/s)", rd, jd, f"mb_s_{n}streams_agg", 0, better="high"))

    out += ["", "**Transcodificação HLS (H.264, uma qualidade; 6 segmentos = 36 s de vídeo)**", "", "| Sessões | Sistema | OK | 1º segmento (s) | Velocidade por sessão (× tempo real) | CPU máx (%) | RAM máx (MiB) |", "|---|---|---|---|---|---|---|"]
    rt, jt = r.get("E_transcode", {}), j.get("E_transcode", {})
    for key in rt or jt:
        for name, d in (("Rustyfin", rt), ("Jellyfin", jt)):
            x = d.get(key, {})
            out.append(f"| {key.split('_')[0]} | {name} | {x.get('ok_de', 'n/d')} | {f(x.get('first_seg_s'))} | {f(x.get('x_realtime_each'), 2)} | {f(x.get('cpu_pct_max'), 0)} | {f(x.get('mem_mib_max'), 0)} |")
    abr = (r.get("E_transcode_abr") or {}).get("1_sessoes")
    if abr:
        out.append(f"| 1 (ABR, 2 qualidades) | Rustyfin | {abr.get('ok_de')} | {f(abr.get('first_seg_s'))} | {f(abr.get('x_realtime_each'), 2)} | {f(abr.get('cpu_pct_max'), 0)} | {f(abr.get('mem_mib_max'), 0)} |")

    for name, d in (("Rustyfin", r), ("Jellyfin", j)):
        s = d.get("F_soak")
        if s:
            out += ["", f"**Estabilidade ({name}, {s.get('minutos')} min de carga mista)**: RAM em repouso {f(s.get('mem_repouso_antes_mib'), 0)} → {f(s.get('mem_repouso_depois_mib'), 0)} MiB (pico durante a carga {f(s.get('mem_mib_max'), 0)}), "
                    f"{s.get('requisicoes_api')} requisições de API, {s.get('erros')} erros."]
    for name, d in (("Rustyfin", r), ("Jellyfin", j)):
        if d.get("erro_geral"):
            out += ["", f"> {name} falhou durante a execução: {d['erro_geral']}"]
    return "\n".join(out)


def main():
    for path in sys.argv[1:]:
        data = json.load(open(path, encoding="utf-8"))
        for profile in (k for k in data if not k.startswith("_")):
            print(section(profile, data[profile]))
            print()


if __name__ == "__main__":
    main()
