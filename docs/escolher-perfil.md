# Qual perfil de execução escolher

O Rustyfin roda de quatro jeitos, e a diferença entre eles é de **velocidade e de uso da CPU** na transcodificação:

| Perfil | O que é |
|---|---|
| `docker-cpu` | Container Docker, transcodificando no processador (`libx264`) |
| `docker-gpu` | Container Docker com a GPU repassada (Linux, VAAPI) |
| `nativo-cpu` | O binário direto no sistema, no processador |
| `nativo-gpu` | O binário direto no sistema com a GPU (Windows: Quick Sync; Linux: VAAPI) |

Para tocar sem transcodificar (Direct Play) a escolha **não muda nada**: o servidor só entrega o arquivo.

## A resposta curta

| Sua máquina | Perfil recomendado | Por quê |
|---|---|---|
| **Windows com GPU Intel** | **`nativo-gpu`** (sem Docker) | O Docker Desktop não repassa a GPU Intel ao container. Direto no Windows o Quick Sync funciona |
| **Linux com GPU Intel ou AMD** | `docker-gpu` | O Docker repassa `/dev/dri`; `sh tools/gpu-setup.sh` liga. (Validado só nos comandos, ainda não numa GPU Linux real) |
| **Sem GPU, ou só NVIDIA** | `docker-cpu` | O Docker perde pouco para o nativo (veja abaixo) e é bem mais simples |
| **Só reproduz sem transcodificar** | `docker-cpu` | Tanto faz; o Docker é o mais fácil |

Se a sua máquina é diferente ou você quer números, **rode o script** (abaixo). Ele mede e recomenda.

## Medido numa máquina real

Notebook Windows 11, Intel Core i7-11800H (16 threads) com GPU Intel UHD (e uma NVIDIA RTX 3060, não usada), Docker
Desktop. Teste: 30 s de vídeo H.264 1080p reduzidos para 720p a 4 Mbps, mediana de 3 repetições
([saída completa](../tools/perfil/exemplos/i7-11800h-uhd-windows.json)).

| Perfil | Velocidade | Núcleos de CPU ocupados | Situação |
|---|---|---|---|
| `docker-cpu` | 5,1× o tempo real | 11,8 de 16 | funciona |
| `docker-gpu` | | | **indisponível**: o Docker Desktop não repassa a GPU Intel |
| `nativo-cpu` | 5,2× | 14,5 de 16 | funciona |
| **`nativo-gpu`** | **8,2×** | **0,6 de 16** | **o melhor** |

- **Quick Sync (nativo) é 1,6× mais rápido e usa cerca de 20 vezes menos CPU.** Deixa a máquina livre para outras coisas e
  atende mais telas ao mesmo tempo.
- **Docker x nativo, no processador, é praticamente igual** (5,1× contra 5,2×): o Docker custa cerca de 3% de velocidade. Por
  isso, sem GPU, vale ficar no Docker.
- **Nesta máquina, a escolha certa é rodar o Rustyfin direto no Windows com Quick Sync**, não pelo Docker.

A GPU não ganha em tudo. Com fontes AV1 ou com legendas de imagem queimadas, a decodificação ou o filtro ficam na CPU e o
ganho de velocidade some numa CPU forte (sobra a economia de CPU). Veja a tabela em
[reprodução](reproducao.md#windows). O script mede o caso mais comum, o H.264 de 1080p.

## Como medir na sua máquina

Precisa de Python 3.8+ e, para o Docker, da imagem do Rustyfin (`docker compose build`). Para os perfis nativos, de um
`ffmpeg` (no Windows, um build `win64-gpl` do projeto `FFmpeg-Builds`, com `h264_qsv`).

```bash
# Linux ou macOS (usa o ffmpeg do PATH e o Docker)
python tools/perfil/escolher_perfil.py

# Windows (aponte a pasta do ffmpeg.exe)
python tools/perfil/escolher_perfil.py --ffmpeg-dir C:\ffmpeg\bin

# Simula um NAS: limita o container a 2 CPUs
python tools/perfil/escolher_perfil.py --docker-cpus 2

# Só o JSON, para um script ou uma LLM ler (veja o guia para LLM)
python tools/perfil/escolher_perfil.py --json --out resultado.json
```

O que ele faz, sem alterar nada do seu sistema: gera um vídeo de teste numa pasta temporária, mede cada perfil que a
máquina permitir (uma rodada de aquecimento e três medidas, em mediana) e imprime a tabela e a recomendação. Leva cerca de
dois minutos. O Docker e a GPU que não existirem aparecem como "indisponível", com o motivo.

Como ler a saída:

| Campo | Significado |
|---|---|
| Velocidade (`x_tempo_real`) | Quantos segundos de vídeo saem por segundo de relógio. 5× é cinco vezes mais rápido que o tempo real |
| Núcleos ocupados | CPU gasta pelo `ffmpeg` (soma de todos os núcleos). Quanto menor, mais sobra para o resto |
| Variação | Diferença entre a melhor e a pior repetição. Acima de 15%, a máquina estava ocupada: rode de novo |

## Como aplicar o perfil escolhido

- **`docker-cpu`:** [primeiros passos](primeiros-passos.md). Nenhuma configuração extra.
- **`docker-gpu` (Linux):** `sh tools/gpu-setup.sh` e `docker compose up -d`.
  Detalhes em [aceleração por hardware](reproducao.md#aceleração-por-hardware).
- **`nativo-gpu` ou `nativo-cpu` (Windows):** [reprodução → Windows](reproducao.md#windows). Em resumo: compile com
  `cargo build --release`, aponte `RUSTYFIN_FFMPEG` para o `ffmpeg.exe` e rode o binário. O encoder `auto` escolhe o Quick
  Sync. Confira no log a linha `Aceleração por hardware ativa: h264_qsv`.

## Limites desta medição

- É **um vídeo sintético** de 1080p H.264, uma transcodificação por vez. Várias sessões, HEVC, AV1 e legendas queimadas se
  comportam de outro jeito; o desempenho completo está em [desempenho](desempenho.md).
- A CPU medida é a do processo `ffmpeg`. O trabalho que o driver da GPU faz fora dele não entra; por isso o uso do sistema
  num teste completo (cerca de 21%) é maior que os 0,6 núcleos acima.
- NVIDIA não é medida: a máquina de teste a tem, mas o foco do projeto é Intel e AMD.
- O número de sessões que o servidor aceita ao mesmo tempo é um limite à parte (`RUSTYFIN_MAX_TRANSCODES`, padrão 4 com GPU
  e metade das CPUs sem ela).

Quer que uma LLM faça isso por você? Entregue a ela o [guia para LLM](guia-llm.md).
