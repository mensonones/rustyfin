# Comparativo de desempenho (Rustyfin × Jellyfin)

`bench.py` roda os dois servidores em containers Docker com os mesmos limites de CPU e memória, um de cada vez, na
mesma biblioteca (montada somente leitura), e mede pegada de recursos, varredura (inclusive em escala), API, Direct
Play, transcodificação HLS e estabilidade. `report.py` transforma os JSON em tabelas Markdown. Os resultados e a
interpretação estão em [docs/desempenho.md](../../docs/desempenho.md).

Requisitos: Python 3.8+, Docker, a imagem do Rustyfin e `jellyfin/jellyfin:latest`. Só usa a biblioteca padrão.

```bash
# Fixe a versão a medir: a imagem :latest muda a cada build
git archive v0.16.0 | docker build -t rustyfin-bench:v0.16.0 -

python tools/bench/bench.py --profile folgado --rustyfin-image rustyfin-bench:v0.16.0 \
    --media /caminho/da/biblioteca --scale 1000,3000 --soak-min 5 --out tools/bench/results/folgado.json
python tools/bench/bench.py --profile restrito --rustyfin-image rustyfin-bench:v0.16.0 \
    --media /caminho/da/biblioteca --scenarios CDE --scale "" --transcode-sessions 1 --reps 3 --soak-min 0 \
    --out tools/bench/results/restrito.json

# Cada etapa é gravada em --out assim que termina. Se a execução cair ou for interrompida, retome com --resume
# (repete só o que falta: cenários C a F e a escala que ainda não foram medidos sem erro).

python tools/bench/report.py tools/bench/results/folgado.json tools/bench/results/restrito.json
```

`--media` deve ter as pastas `filmes/` e `series/`. Perfis: `folgado` (4 CPUs, 4 GB) e `restrito` (1 CPU, 512 MB).
Cenários (`--scenarios`): **A** pegada, **B** biblioteca e escala, **C** API, **D** Direct Play, **E** transcodificação,
**F** estabilidade (a pegada e a varredura inicial sempre rodam).

## Cuidados para uma medição válida

- Rode com a máquina quieta: outras compilações, builds do Docker e sessões de trabalho no mesmo repositório distorcem os números.
- Fixe a imagem (`--rustyfin-image`), senão um build no meio da rodada troca a versão medida.
- O harness aquece o cache de arquivos dentro da VM do Docker; em Windows com WSL2, a primeira leitura de um disco montado é muito lenta.
- Uma rodada completa leva cerca de uma hora e meia.
