# Desempenho: Rustyfin × Jellyfin

Comparativo medido em **17/11/2026** com o Rustyfin **v0.16.0** (os cenários de API e de transcodificação foram refeitos na **v0.17.4**) e o Jellyfin **12.4.0** (imagem `latest` do dia),
com o *harness* que está em [`tools/bench/`](../tools/bench/README.md) e os resultados brutos em
`tools/bench/results/`. Os números são de uma máquina e de uma rodada completa, não um estudo estatístico: leia as
[ressalvas](#ressalvas) antes de citá-los.

## Resumo

No perfil **folgado** (4 CPUs, 4 GB por container) e na mesma biblioteca:

| | Rustyfin | Jellyfin | |
|---|---|---|---|
| Memória em repouso (biblioteca indexada) | **11 MiB** | 391 MiB | 36× menos |
| Imagem Docker | **790 MB** | 2.490 MB | 3,2× menor |
| Subida até a API responder | **0,6 s** | 6,0 s | 10× mais rápido |
| API, 1 conexão (p50) | **1,5 a 2,5 ms** | 12 a 25 ms | 6 a 15× |
| API, 50 conexões, `Items/{id}` | **2.567 req/s** | 67 req/s | 38× |
| Direct Play, 1 stream | **185 MB/s** | 78 MB/s | 2,4× |
| Direct Play, 20 streams (agregado) | 228 MB/s | 206 MB/s | 1,1× |
| Rescan sem mudanças | **0,13 s** | 6,1 s | 45× |
| Varredura de 1.000 clipes em uma pasta | **21,9 s** | 412,7 s | 19× |
| Varredura de 3.000 clipes em uma pasta | **62,3 s** | não terminou em 15 min | |
| Arquivo novo aparece (1.000 clipes) | **1,0 s** | 3,9 s | 4× |
| 1ª varredura de 18 vídeos reais (~30 GB) | **3,0 s** (v0.17.3; era 286 s na v0.16.0) | 58 s | 19× |
| **Transcodificação, 1 sessão (4 CPUs)** | 13,4× tempo real (v0.17.4; era 2,97×) | **20,5×** | **Jellyfin 1,5× mais rápido** |
| **Transcodificação, 4 sessões simultâneas** | 4 de 4 concluíram, 1,4× cada (v0.17.4; antes 2 de 4) | 4 de 4, **2,3× cada** | **Jellyfin 1,6× mais rápido** |

No perfil **restrito** (1 CPU, 512 MB) o Rustyfin segue na frente em tudo o que foi medido, inclusive na transcodificação
de 1 sessão (3,07× contra 0,95× tempo real, na v0.17.4). A 1ª varredura desse perfil (188 s) também foi medida na v0.16.0, antes da
correção descrita abaixo.

## Onde o Rustyfin perde (e por quê)

1. ~~**1ª varredura de uma biblioteca de arquivos grandes: 286 s contra 58 s.**~~ **Corrigido na v0.17.3.** Era uma
   regressão da v0.13.0: a funcionalidade que preserva o histórico ao mover arquivos calculava o **SHA-256 do arquivo
   inteiro** durante a varredura, ou seja, lia os 30 GB. Agora a varredura só calcula o hash quando ele decide algo
   (arquivo alterado que já tinha hash, ou arquivo novo com o tamanho ou o nome de um item que sumiu) e o restante é
   preenchido em segundo plano, um arquivo por vez. Medido depois da correção, a mesma varredura leva **3,0 s**
   (1,0 s até os itens aparecerem), com o resultado completo em segundo plano. O custo é uma janela curta depois da
   primeira varredura em que um arquivo movido ainda não tem hash: ele não é movido sozinho e vai para a fila de revisão
   do acervo, em vez de perder o histórico em silêncio.
2. **Transcodificação por sessão.** Com 4 CPUs o Jellyfin ainda é mais rápido (20,5× contra 13,4× com 1 sessão; 2,3× contra
   1,4× com 4). Na v0.17.4 o x264 passou a usar os mesmos ajustes de busca de movimento do Jellyfin (`subme=0`, `me=hex`,
   `rc_lookahead=10`), o que levou 1 sessão de 2,97× para 13,4×; a diferença restante não foi investigada. As **4 sessões
   simultâneas** que falhavam (2 de 4, `playlist sem segmentos`) foram corrigidas: o ffmpeg de uma sessão encerrada seguia
   ocupando a vaga até a limpeza de 90 s e o cliente recebia 503. Agora `Sessions/Playing/Stopped` libera a vaga na hora e,
   com o limite cheio, a sessão parada há mais de 12 s cede a vaga. No perfil restrito (1 CPU) o Rustyfin segue na frente.
3. **Reinício a quente: 3,8 s contra 2,6 s** no perfil folgado (no restrito o Rustyfin ganha: 3,7 s contra 6,2 s). O
   tempo inclui o `docker restart`, que espera o encerramento do processo.

## O que o Rustyfin faz melhor (e por quê)

- **Memória e imagem:** binário único em Rust, sem coleta de lixo e sem a pilha .NET.
- **API:** consultas simples ao SQLite, sem camada de serviços entre a rota e o banco. A diferença cresce com a
  concorrência.
- **Direct Play:** leitura em blocos de 256 KB sem copiar o arquivo para a memória.
- **Varredura de muitos arquivos pequenos:** cada pasta é listada uma única vez (antes de 5490b99, listar a pasta por
  vídeo tornava a varredura quadrática: 1.000 clipes levavam 212 s e 3.000 não terminavam).

## Metodologia

- **Máquina:** i7-11800H (8 núcleos, 16 threads), 32 GB de RAM, Docker Desktop com WSL2 (16 CPUs e 15,5 GiB visíveis).
- **Um sistema por vez**, cada um num container com os mesmos limites: **folgado** (`--cpus 4 --memory 4g`) e **restrito**
  (`--cpus 1 --memory 512m`). Volume de dados novo em cada subida.
- **Mesma biblioteca**, montada somente leitura: 18 vídeos reais (~30 GB, 8 filmes e 1 série) e, para a escala, 1.000 e
  3.000 clipes de 2 s (cópias de um mesmo arquivo) numa única pasta.
- **Sem metadados online** nos dois (compara o motor, não a rede). No Jellyfin: sem provedores da internet, sem monitor
  em tempo real, sem imagens de capítulos e sem *trickplay*. No Rustyfin: sem o monitor de pastas.
- **Cache de arquivos aquecido** dentro da VM do Docker antes de cada sistema (o começo e o fim de cada vídeo, e o filme
  usado nos testes de streaming), para que o primeiro a rodar não pague a leitura fria do disco do Windows.
- **Repetições:** 4 rodadas, descartando a primeira, e a mediana das 3 restantes (API e streaming); 3 rodadas na
  transcodificação.
- **Transcodificação:** o perfil do cliente limita o bitrate a 700 kbps (abaixo do arquivo, que tem ~1 Mbps) para os dois
  **recodificarem** o vídeo H.264 para HLS em uma qualidade. Sem isso o Jellyfin apenas copia o vídeo, o que não é o mesmo
  trabalho. Cada sessão baixa 6 segmentos de 6 s; a "velocidade" é 36 s de vídeo dividido pelo tempo total. O Jellyfin usa
  o `jellyfin-ffmpeg` próprio dele; o Rustyfin usa o `ffmpeg` da imagem Debian.
- **Estabilidade:** 5 minutos de carga mista (2 leitores de arquivo, navegação pela API e uma transcodificação em
  repetição). A memória é medida **em repouso, antes e depois**, porque durante a carga o `docker stats` soma os
  processos do `ffmpeg`.
- **Varredura:** o tempo conta desde o pedido de varredura até o número esperado de itens aparecer na API (e até a
  tarefa terminar).

## Ressalvas

- **Uma máquina, uma rodada completa** (mais as correções descritas abaixo). Há variação entre execuções: em outras
  rodadas o Direct Play do Rustyfin ficou entre 132 e 195 MB/s e o do Jellyfin entre 57 e 95 MB/s.
- Duas outras sessões de trabalho mexiam no mesmo repositório e reconstruíram a imagem `latest` durante as primeiras
  tentativas, e uma varredura fria do disco montado do Windows foi extremamente lenta. As primeiras rodadas foram
  descartadas; os números acima vêm de uma rodada final com a imagem fixada em `rustyfin-bench:v0.16.0` e o cache aquecido.
- A "RAM" é a do cgroup do container (`docker stats`), que inclui processos filhos como o `ffmpeg`. Com 1 CPU e 512 MB a
  transcodificação encosta no limite de memória nos dois.
- O "reinício a quente" inclui o tempo de o Docker encerrar o processo.
- O Jellyfin não terminou a varredura de 3.000 clipes em 15 minutos (limite do teste); o número real é maior.
- Não medido: legendas, HDR, ABR comparável (o Jellyfin tem uma qualidade só nesse perfil), TMDB, bibliotecas com
  dezenas de milhares de itens e estabilidade de horas. O Jellyfin faz mais por arquivo na varredura (imagens, banco),
  então o trabalho não é idêntico.

## Resultados completos

<details>
<summary>Tabelas dos perfis folgado e restrito</summary>

### Perfil folgado

| Métrica | Rustyfin | Jellyfin | Vantagem do Rustyfin |
|---|---|---|---|
| Imagem Docker (MB) | 790 | 2 490 | 3,2× |
| Subida até responder (s, volume novo) | 0,6 | 6,0 | 10,5× |
| Reinício a quente (s) | 3,8 | 2,6 | 0,7× |
| RAM em repouso, biblioteca vazia (MiB) | 5 | 191 | 40,9× |
| RAM em repouso, biblioteca indexada (MiB) | 11 | 391 | 36,5× |
| CPU em repouso, indexada (%) | 0,0 | 0,5 |  |

**Biblioteca**

| Métrica | Rustyfin | Jellyfin | Vantagem |
|---|---|---|---|
| 1ª varredura de 18 vídeos reais: visíveis na API (s) | 286,2 | 38,5 | 0,1× |
| 1ª varredura: concluída (s) | 286,2 | 57,7 | 0,2× |
| Pico de RAM na varredura (MiB) | 121 | 462 | 3,8× |
| Rescan sem mudanças (s) | 0,13 | 6,07 | 45,5× |
| Varredura de 1000 clipes (s) | 21,9 | 412,7 | 18,9× |
|   pico de RAM com 1000 clipes (MiB) | 64 | 289 | 4,5× |
|   listar 50 itens com 1000 clipes (ms, p50) | 4,5 | 31,0 | 6,8× |
| Varredura de 3000 clipes (s) | 62,3 | n/d |  |
|   pico de RAM com 3000 clipes (MiB) | 66 | 352 | 5,3× |
|   listar 50 itens com 3000 clipes (ms, p50) | 6,1 | n/d |  |
| Arquivo novo aparece (s, com 1000 clipes) | 1,0 | 3,9 | 4,0× |

**API (p50 em ms e requisições por segundo; mediana de 3 rodadas)**

| Rota | Conexões | Rustyfin p50 | Jellyfin p50 | Rustyfin req/s | Jellyfin req/s |
|---|---|---|---|---|---|
| `UserViews` | 1 | 1,5 | 17,1 | 271 | 45 |
| `Items?Recursive` | 1 | 2,2 | 29,3 | 179 | 38 |
| `Items/{id}` | 1 | 1,4 | 31,8 | 249 | 30 |
| `Latest` | 1 | 2,1 | 17,1 | 186 | 45 |
| `Shows/Seasons` | 1 | 1,7 | 12,1 | 209 | 56 |
| `UserViews` | 10 | 3,2 | 46,1 | 2 911 | 193 |
| `Items?Recursive` | 10 | 5,3 | 55,1 | 1 433 | 171 |
| `Items/{id}` | 10 | 3,1 | 114,2 | 2 903 | 81 |
| `Latest` | 10 | 6,1 | 46,0 | 1 189 | 208 |
| `Shows/Seasons` | 10 | 3,4 | 35,0 | 2 442 | 257 |
| `UserViews` | 50 | 7,0 | 242,2 | 2 702 | 196 |
| `Items?Recursive` | 50 | 31,3 | 293,0 | 1 108 | 164 |
| `Items/{id}` | 50 | 7,1 | 840,6 | 2 795 | 54 |
| `Latest` | 50 | 49,4 | 325,7 | 808 | 138 |
| `Shows/Seasons` | 50 | 7,8 | 232,6 | 2 455 | 198 |

**Direct Play**

| Métrica | Rustyfin | Jellyfin | Vantagem |
|---|---|---|---|
| Tempo até o 1º byte (ms) | 4,2 | 13,7 | 3,3× |
| Vazão de 1 stream (MB/s) | 185 | 78 | 2,4× |
| Seek de 64 KB, p50 (ms) | 4,4 | 9,2 | 2,1× |
| Seek de 64 KB, p95 (ms) | 23,4 | 28,6 | 1,2× |
| 5 streams simultâneos, vazão agregada (MB/s) | 246 | 200 | 1,2× |
| 10 streams simultâneos, vazão agregada (MB/s) | 226 | 205 | 1,1× |
| 20 streams simultâneos, vazão agregada (MB/s) | 228 | 206 | 1,1× |

**Transcodificação HLS (H.264, uma qualidade; 6 segmentos = 36 s de vídeo)**

| Sessões | Sistema | OK | 1º segmento (s) | Velocidade por sessão (× tempo real) | CPU máx (%) | RAM máx (MiB) |
|---|---|---|---|---|---|---|
| 1 | Rustyfin | 1/1 | 0,6 | 13,42 | 360 | 247 |
| 1 | Jellyfin | 1/1 | 0,7 | 20,45 | 389 | 683 |
| 2 | Rustyfin | 2/2 | 1,7 | 4,62 | 395 | 475 |
| 2 | Jellyfin | 2/2 | 2,0 | 5,81 | 403 | 955 |
| 4 | Rustyfin | 4/4 | 4,8 | 1,39 | 412 | 938 |
| 4 | Jellyfin | 4/4 | 4,9 | 2,29 | 412 | 1 473 |
| 1 (ABR, 2 qualidades) | Rustyfin | 1/1 | 0,6 | 14,19 | 344 | 248 |

**Estabilidade (Rustyfin, 5 min de carga mista)**: RAM em repouso 54 → 53 MiB (pico durante a carga 1 486), 1187 requisições de API, 0 erros.

**Estabilidade (Jellyfin, 5 min de carga mista)**: RAM em repouso 398 → 392 MiB (pico durante a carga 1 666), 450 requisições de API, 0 erros.

### Perfil restrito

| Métrica | Rustyfin | Jellyfin | Vantagem do Rustyfin |
|---|---|---|---|
| Imagem Docker (MB) | 790 | 2 490 | 3,2× |
| Subida até responder (s, volume novo) | 0,6 | 3,9 | 6,2× |
| Reinício a quente (s) | 3,7 | 6,2 | 1,7× |
| RAM em repouso, biblioteca vazia (MiB) | 4 | 180 | 43,0× |
| RAM em repouso, biblioteca indexada (MiB) | 7 | 374 | 50,0× |
| CPU em repouso, indexada (%) | 0,0 | 0,4 |  |

**Biblioteca**

| Métrica | Rustyfin | Jellyfin | Vantagem |
|---|---|---|---|
| 1ª varredura de 18 vídeos reais: visíveis na API (s) | 188,5 | 37,8 | 0,2× |
| 1ª varredura: concluída (s) | 188,5 | 56,0 | 0,3× |
| Pico de RAM na varredura (MiB) | 156 | 444 | 2,8× |
| Rescan sem mudanças (s) | 0,15 | 7,53 | 48,9× |

**API (p50 em ms e requisições por segundo; mediana de 3 rodadas)**

| Rota | Conexões | Rustyfin p50 | Jellyfin p50 | Rustyfin req/s | Jellyfin req/s |
|---|---|---|---|---|---|
| `UserViews` | 1 | 3,1 | 17,1 | 232 | 44 |
| `Items?Recursive` | 1 | 2,5 | 29,1 | 207 | 38 |
| `Items/{id}` | 1 | 1,5 | 38,8 | 348 | 28 |
| `Latest` | 1 | 2,4 | 29,1 | 216 | 41 |
| `Shows/Seasons` | 1 | 1,6 | 17,6 | 321 | 47 |
| `UserViews` | 10 | 3,1 | 135,3 | 2 085 | 70 |
| `Items?Recursive` | 10 | 6,6 | 151,2 | 736 | 63 |
| `Items/{id}` | 10 | 3,2 | 211,0 | 2 216 | 45 |
| `Latest` | 10 | 7,4 | 121,4 | 609 | 77 |
| `Shows/Seasons` | 10 | 4,1 | 100,2 | 1 348 | 97 |
| `UserViews` | 50 | 12,2 | 678,6 | 1 951 | 73 |
| `Items?Recursive` | 50 | 56,9 | 749,9 | 766 | 67 |
| `Items/{id}` | 50 | 7,3 | 997,7 | 2 302 | 49 |
| `Latest` | 50 | 84,8 | 609,3 | 610 | 81 |
| `Shows/Seasons` | 50 | 14,5 | 478,9 | 1 587 | 103 |

**Direct Play**

| Métrica | Rustyfin | Jellyfin | Vantagem |
|---|---|---|---|
| Tempo até o 1º byte (ms) | 4,9 | 12,1 | 2,5× |
| Vazão de 1 stream (MB/s) | 195 | 91 | 2,1× |
| Seek de 64 KB, p50 (ms) | 3,9 | 9,6 | 2,4× |
| Seek de 64 KB, p95 (ms) | 27,7 | 30,7 | 1,1× |
| 5 streams simultâneos, vazão agregada (MB/s) | 272 | 206 | 1,3× |
| 10 streams simultâneos, vazão agregada (MB/s) | 252 | 202 | 1,2× |
| 20 streams simultâneos, vazão agregada (MB/s) | 209 | 194 | 1,1× |

**Transcodificação HLS (H.264, uma qualidade; 6 segmentos = 36 s de vídeo)**

| Sessões | Sistema | OK | 1º segmento (s) | Velocidade por sessão (× tempo real) | CPU máx (%) | RAM máx (MiB) |
|---|---|---|---|---|---|---|
| 1 | Rustyfin | 1/1 | 2,2 | 3,07 | 100 | 245 |
| 1 | Jellyfin | 1/1 | 22,1 | 0,95 | 104 | 512 |
| 1 (ABR, 2 qualidades) | Rustyfin | 1/1 | 2,1 | 3,29 | 100 | 246 |

</details>

---

## Metodologia proposta (roteiro completo)

O restante deste documento é o roteiro original, com cenários que ainda não foram medidos.

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
