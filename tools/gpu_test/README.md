# Testes do transcode por GPU

`gpu_test.py` sobe o Rustyfin **fora do Docker** (um binário seu, com um SQLite temporário), gera vídeos de teste com o
`ffmpeg` e confere, pela API, o transcode HLS com a GPU. Serve para validar o Quick Sync (Windows) e o VAAPI (Linux) numa
máquina real, onde o Docker Desktop não repassa a GPU. Os resultados medidos numa Intel UHD estão em
[docs/reproducao.md](../../docs/reproducao.md#aceleração-por-hardware).

Requisitos: Python 3.8+, um binário do Rustyfin (`cargo build --release`) e um `ffmpeg` com o encoder da sua GPU
(`h264_qsv` ou `h264_vaapi`), mais `libx264`, `libx265`, `libvpx-vp9` e `libsvtav1` para gerar os vídeos (o que faltar é
pulado). No Windows, os builds `win64-gpl` do projeto `FFmpeg-Builds` servem. Usa só a biblioteca padrão e o `bench.py`.

```bash
cargo build --release

# Windows (a pasta com ffmpeg.exe e ffprobe.exe)
python tools/gpu_test/gpu_test.py --ffmpeg-dir C:\ffmpeg\bin

# Linux (ffmpeg e ffprobe do PATH, GPU em /dev/dri)
python tools/gpu_test/gpu_test.py

# Comparar com o processador
python tools/gpu_test/gpu_test.py --encoder libx264
```

Opções: `--exe` (binário do Rustyfin; padrão `target/release` ou `target/debug`), `--ffmpeg-dir`, `--encoder` (`auto`,
`libx264`, `h264_qsv`, `h264_vaapi`), `--suite` (`formatos`, `extra`, `falha` ou `todos`), `--porta`, `--clips-dir` (os
vídeos gerados são reaproveitados entre as execuções) e `--limpar` (apaga-os no fim). O código de saída é 1 se algum caso
falhar.

## O que é testado

| Grupo | Casos |
|---|---|
| `formatos` | H.264 de 8 e 10 bits, HEVC de 8 e 10 bits, HDR10 (tone mapping), VP9; segmentos HLS de 6 s exatos, avanço, ABR com duas qualidades, fMP4, stream progressivo e três sessões simultâneas (com a CPU do sistema) |
| `extra` | 4K (H.264 e HEVC), AV1, H.264 entrelaçado e legenda de imagem (PGS) queimada em H.264 e em HEVC, conferida pelos pixels do vídeo |
| `falha` | Um `ffmpeg` que falha quando a sessão pede a GPU: o servidor deve concluir a sessão em software e registrar o aviso |

Os vídeos são sintéticos (1080p, ou 4K nos casos 4K). A legenda PGS é escrita pelo próprio script, num arquivo `.sup`.
Sem GPU ativa no servidor (por exemplo com `--encoder libx264`) o grupo `falha` é pulado: não há falha de GPU para simular.
Cada caso imprime `OK`, `FALHOU` ou `PULOU` (arquivo de teste que o `ffmpeg` não conseguiu gerar, ou grupo sem GPU).

O servidor só vê o `ffprobe` se ele estiver no `PATH`: o script o acrescenta a partir de `--ffmpeg-dir`.
