# ⚖️ Proposta: comparativo de performance Rustyfin × Jellyfin

> Status: **executado em parte** (2026-11-17): footprint, varredura, API, imagens e Direct Play. Transcode, escala (1k–10k arquivos), TMDB e soak **não** foram medidos. O restante deste arquivo é a proposta original.

## Resultados

Mesma máquina, mesma biblioteca (18 vídeos, ~30 GB, montada `:ro`), cada servidor em container com 4 CPUs / 4 GB, config limpa, **sem metadados online**, cliente Python local, 1 rodada (sem repetições/mediana: indicativo, não estatístico). Jellyfin 12.4.0 contra Rustyfin (build desta data).

| Métrica | Rustyfin | Jellyfin |
|---|---|---|
| Imagem Docker | 785 MB | 2,49 GB |
| RAM após indexar e ser usado | ~16 MiB | ~440 MiB |
| Varredura inicial (18 arquivos) | **0,55 s** | 52,7 s |
| `UserViews` / `Items` (1 conexão, p50) | 1,7 / 2,7 ms | 29 / 30 ms |
| `Items/{id}` com 50 conexões (req/s, p50) | ~2.970 req/s, 6 ms | ~72 req/s, 658 ms |
| `Items?Recursive` com 50 conexões | ~1.500 req/s, 21 ms | ~170 req/s, 274 ms |
| Direct Play: vazão de 1 stream | 183 MB/s | 95 MB/s |
| Direct Play: 5 streams (agregado) | 258 MB/s | ~213–233 MB/s |
| Seek `Range` 64 KB (p50 / p95) | 3,9 / 26 ms | 16 / 32 ms |
| Tempo até o 1º byte | ~17 ms | ~33–72 ms |

Ressalvas importantes:
- O Direct Play do Rustyfin estava em **22 MB/s** (4× pior que o Jellyfin) por causa de leitura em blocos de 4 KB. Corrigido para 256 KB durante este teste; a tabela mostra o valor após a correção.
- **Imagens:** o filme medido (The Odyssey) não tem arte local, então os dois servidores usam um quadro do vídeo; o Jellyfin o gera durante a varredura (original ~790 KB, ~30 ms). O Rustyfin gerava sob demanda (~180 ms no 1º acesso); agora **pré-gera em segundo plano após a varredura** e o 1º acesso leva **~3 ms**. Redimensionado (`maxWidth=300`) o Rustyfin entrega ~4 KB (Jellyfin ~29 KB), mas a 1ª geração de cada tamanho ainda custa ~80–100 ms (depois fica em cache).
- O Jellyfin faz bem mais por arquivo na varredura (imagens, chapters, banco), e o tempo foi medido por polling da tarefa agendada; não é "trabalho idêntico".
- Máquina e Docker Desktop/WSL2 com bind mount do Windows: leitura de disco ruidosa; a ordem alternada reduz, mas não elimina, efeito de cache.
- Não medido: transcodificação, legendas, ABR, aceleração de hardware, TMDB, escala e estabilidade longa (onde o Jellyfin tem mais recursos).

## 1. Ambiente e ponto de partida

| | Rustyfin | Jellyfin (já na máquina) |
|---|---|---|
| Imagem Docker | `rustyfin-rustyfin` — **785 MB** (Debian + ffmpeg) | `jellyfin/jellyfin:latest` — **2,49 GB** |
| RAM em repouso (medido agora) | **15,5 MiB** | *a medir* |
| Estado | rodando (porta 8096) | parado (`Exited 137`, sem OOM), portas 8096/7359/1900/8920 |
| Dados | `./data` (5,5 MB), `./media` | `…\.gemini\antigravity\scratch\jellyfin\{config,cache,media}` |

* **Máquina:** i7‑11800H (8 núcleos/16 threads), 32 GB de RAM, Docker Desktop (WSL2) com 16 CPUs e 15,5 GiB visíveis.
* **Biblioteca do Jellyfin** (pasta `media`): **23 arquivos, ~30,5 GB** (18 `.mp4` de 0,9 a 4 GB, 4 `.jpg`, 1 `.nfo`);
  `filmes/` (Kingsman, The Odyssey…) e `series/` (Namorado por Assinatura, S01E01‑E02…), com `poster.jpg`, `fanart.jpg`,
  `tvshow.nfo`, pastas `Season 01` e etiqueta `[tmdbid-…]`. É um bom conjunto real, e ainda testa o parser com os nomes do Jellyfin.
* O Jellyfin já tem biblioteca/metadados prontos (`config` ≈ 240 MB: metadata 72 MB, plugins 157 MB).

## 2. Princípios para a comparação ser justa

1. **Mesma biblioteca, somente leitura:** a mesma pasta `media` montada `:ro` nos dois. Nada na pasta original do Jellyfin é alterado.
2. **Um por vez** (as portas conflitam) e **recursos idênticos** por container (`--cpus`, `--memory`), em dois perfis:
   *folgado* (4 CPUs / 4 GB) e *restrito* (1 CPU / 512 MB, o argumento de "leve").
3. **Configuração do Jellyfin em cópia:** para "primeira varredura" usa-se uma config **vazia**; para "a quente" uma **cópia** da config existente. A original nunca é montada.
4. **Versões fixadas e registradas** (tag exata do Jellyfin via `/System/Info/Public`; data/versão do build do Rustyfin).
5. **Mesmo ffmpeg lógico no transcode:** libx264 (CPU), mesmo `preset`/`crf`/bitrate, sem aceleração de hardware. Registrar que o Jellyfin usa o seu próprio `jellyfin-ffmpeg`.
6. **Repetições:** 5 execuções por cenário, descartando a 1ª; reportar **mediana e p95**; intercalar a ordem dos sistemas; pausa de 30 s entre rodadas.
7. **Autenticado nos dois** (ambos exigem token): mesmo usuário/estrutura de requisição.
8. **Metadados online separados do núcleo:** cada cenário roda com *metadados locais apenas* (compara o motor) e, à parte, com *TMDB ligado* (depende de rede, é secundário).
9. **Cuidados de I/O:** bind mount do Windows (`C:\`→WSL2) é lento e ruidoso para ambos; repetir os cenários de leitura com uma cópia da mídia dentro do WSL2 (ext4) e reportar os dois. Registrar cache de disco (execução "a frio" vs "a quente").

## 3. Cenários e métricas

**A. Pegada de recursos**
- Tamanho da imagem; tempo de subida até `/System/Info/Public` = 200 (frio/quente).
- RAM e CPU em repouso (mediana de 60 s) com biblioteca vazia e com a biblioteca de 30 GB indexada.
- Tamanho em disco do banco/config/cache depois da varredura.

**B. Biblioteca**
- Primeira varredura completa (banco vazio): tempo até os 23 arquivos aparecerem na API, pico de RAM/CPU.
- Rescan sem mudanças; e latência para um **arquivo novo** aparecer (copiar um mp4 e medir até `Items`).
- *Escala:* biblioteca sintética de 1k/5k/10k arquivos (clips minúsculos) → tempo de varredura e latência de listagem por tamanho.

**C. API (o que o cliente sente)**
- Gerador de carga (`oha`/`hey` em container na mesma rede Docker) com 1, 10 e 50 conexões:
  `UserViews`, `Items?Recursive` (paginado), `Items/{id}`, `Latest`, `Resume`, `Shows/{id}/Seasons|Episodes`, busca e login.
- Métricas: requisições/s, **p50/p95/p99**, erros, RAM durante a carga. Imagem primária (1ª vez × em cache).

**D. Streaming (Direct Play)**
- Vazão de uma leitura contínua de arquivo de 2–4 GB (MB/s) e **tempo até o 1º byte**.
- *Seek:* 1.000 requisições `Range` em posições aleatórias (latência p50/p95).
- 1, 5, 10 e 20 streams simultâneos: vazão agregada, CPU e RAM.

**E. Transcodificação** (só depois de validar que o ffmpeg está equivalente)
- Remux (vídeo copiado + AAC) e transcode H.264 a 5 Mbps: **tempo até o 1º segmento HLS**, velocidade (× tempo real), CPU e RAM, e quantas sessões simultâneas até saturar.

**F. Estabilidade**
- *Soak* de 1 h com carga mista leve (navegação + 2 streams): crescimento de RAM (vazamentos) e erros.

## 4. O que **não** entra (para não distorcer)
Legendas/extração, *trickplay*, capítulos, plugins, SyncPlay, DLNA, múltiplas qualidades (ABR) e aceleração de hardware existem só (ou melhor) no Jellyfin.
O relatório lista isso explicitamente: é um comparativo **das funcionalidades em comum**, não de produtos completos.

## 5. Hipóteses (para poderem ser refutadas)
- Rustyfin vence em **RAM, imagem, tempo de subida, varredura e latência de API** (menos trabalho por requisição).
- **Direct Play ≈ empate** (limitado por disco/rede) e **transcode ≈ empate** (limitado pelo ffmpeg).
- O Jellyfin pode ganhar onde faz mais cache/pré-cálculo (ex.: imagens redimensionadas, listagens repetidas) e em ABR/aceleração.
- Em metadados online o Rustyfin pode ser **mais lento** (chama `curl` por requisição).

## 6. Entrega
1. `tools/bench/` — *harness* (PowerShell/Python): sobe cada sistema com os limites, amostra `docker stats` (1 Hz → CSV), roda os cenários e salva JSON.
2. **Relatório** com tabelas (mediana/p95), gráficos e ressalvas, publicável como página.
3. Esforço estimado: ~½ dia para o *harness* + 2–3 h de execução (A–E) e 1 h de *soak*.

## 7. Decisões necessárias
1. Posso montar a pasta `media` do Jellyfin **somente leitura** nos dois sistemas e usar **cópias** da config? (recomendado)
2. Incluir o cenário com **TMDB ligado** (usa a sua chave; depende da rede)?
3. Perfis de recursos: *folgado* (4 CPU/4 GB) e *restrito* (1 CPU/512 MB)?
4. Posso **parar o Rustyfin** durante as medidas do Jellyfin (portas) e depois religar?
