# Desempenho: Rustyfin × Jellyfin

Comparativo medido em **17 e 18/11/2026** com o Rustyfin **v0.17.5** e o Jellyfin **12.4.0** (imagem `latest` do dia),
com o *harness* que está em [`tools/bench/`](../tools/bench/README.md) e os resultados brutos em
[`tools/bench/results/`](../tools/bench/results/). Os números são de uma máquina e de uma rodada completa, não um estudo
estatístico: leia as [ressalvas](#ressalvas) antes de citá-los.

## Resumo

Mesma biblioteca (18 vídeos reais, ~30 GB), um sistema por vez, em dois perfis de container: **folgado** (4 CPUs,
4 GB) e **restrito** (1 CPU, 512 MB, o caso de um NAS ou mini-PC). Em negrito, quem vai melhor.

| | Folgado: Rustyfin | Folgado: Jellyfin | Restrito: Rustyfin | Restrito: Jellyfin |
|---|---|---|---|---|
| Imagem Docker | **791 MB** | 2.490 MB | **791 MB** | 2.490 MB |
| Subida até a API responder | **0,6 s** | 6,0 s | **0,5 s** | 3,9 s |
| Reinício a quente | **0,8 s** | 2,6 s | **0,6 s** | 6,2 s |
| Memória em repouso (biblioteca indexada) | **11 MiB** | 391 MiB | **9 MiB** | 374 MiB |
| 1ª varredura de 30 GB (concluída) | **3,0 s** | 57,7 s | **3,0 s** | 56,0 s |
| Rescan sem mudanças | **0,14 s** | 6,1 s | **0,11 s** | 7,5 s |
| API, 1 conexão (p50, 5 rotas) | **1,4 a 2,1 ms** | 12 a 32 ms | **1,4 a 2,1 ms** | 17 a 39 ms |
| API, 50 conexões, `Items/{id}` | **2.353 req/s** | 54 req/s | **2.175 req/s** | 49 req/s |
| Direct Play, 1 stream | **182 MB/s** | 78 MB/s | **182 MB/s** | 91 MB/s |
| Direct Play, 20 streams (agregado) | **224 MB/s** | 206 MB/s | **221 MB/s** | 194 MB/s |
| Transcodificação, 1 sessão (× tempo real) | **14,0×** | 12,2× | **3,3×** | 2,6× |
| Transcodificação, 2 sessões simultâneas | **4,6×** cada | 3,2× cada | não medido | não medido |
| Transcodificação, 4 sessões simultâneas | 1,27× cada | 1,27× cada | não medido | não medido |
| Varredura de 1.000 clipes numa pasta | **14,7 s** | 412,7 s | não medido | não medido |
| Varredura de 3.000 clipes numa pasta | **45,7 s** | não terminou | não medido | não medido |
| Arquivo novo aparece (1.000 clipes) | **0,8 s** | 3,9 s | não medido | não medido |
| Memória após 5 min de carga mista | **27 MiB** | 392 MiB | não medido | não medido |

**No perfil restrito o Rustyfin vence em tudo o que foi medido**, e é onde a diferença mais pesa: o Jellyfin usa 374 MiB
em repouso dentro de um limite de 512 MiB, leva 56 s para indexar a mesma biblioteca (contra 3 s) e reinicia em 6,2 s
(contra 0,6 s). No perfil folgado o Rustyfin vence em tudo e só empata na transcodificação com 4 sessões
simultâneas.

## Onde o Rustyfin não ganha

**Transcodificação com 4 sessões simultâneas e 4 CPUs: empate** (1,27× cada). Os dois saturam a CPU com o `libx264`; o
Rustyfin usa menos memória (950 MiB contra 1.507 MiB). Em todo o resto medido o Rustyfin está à frente.

## Correções feitas a partir deste benchmark

O comparativo foi feito para achar pontos fracos, e estes foram corrigidos:

- **1ª varredura de uma biblioteca de arquivos grandes: 286 s contra 58 s (v0.17.3).** Era uma regressão da v0.13.0: a
  funcionalidade que preserva o histórico ao mover arquivos calculava o SHA-256 do arquivo inteiro durante a varredura,
  ou seja, lia os 30 GB. Agora a varredura só calcula o hash quando ele decide algo (arquivo alterado que já tinha hash,
  ou arquivo novo com o tamanho ou o nome de um item que sumiu) e o restante é preenchido em segundo plano, um arquivo
  por vez. O custo é uma janela curta depois da primeira varredura em que um arquivo movido ainda não tem hash: ele não é
  movido sozinho e vai para a fila de revisão do acervo, em vez de perder o histórico em silêncio.
- **Quatro sessões HLS simultâneas: 2 de 4 falhavam (v0.17.4).** O ffmpeg de uma sessão encerrada seguia ocupando a vaga
  até a limpeza de 90 s, e o cliente recebia 503. Agora `Sessions/Playing/Stopped` libera a vaga na hora e, com o limite
  cheio, uma sessão parada há mais de 12 s cede a vaga.
- **Velocidade de transcodificação: 2,97× com 1 sessão (v0.17.4).** O x264 passou a usar os mesmos ajustes de busca de
  movimento do Jellyfin (`subme=0`, `me=hex`, `rc_lookahead=10`), chegando a 14,0×. Testei também `-sc_threshold 0` e o
  perfil Main, que o Jellyfin usa: não aceleram neste arquivo e não foram adotados.
- **Reinício a quente: 3,6 s contra 2,6 s do Jellyfin (v0.17.5).** O servidor não tratava o `SIGTERM`: como PID 1 no
  container ele o ignorava, e o `docker stop` só o encerrava com `SIGKILL` depois do prazo (código de saída 137). A subida
  levava só 0,55 s; o resto era a espera. Agora `SIGTERM` e `SIGINT` encerram as transcodificações, fecham o banco e saem
  com código 0, e o reinício caiu para 0,8 s.
- **Varredura de muitas pastas pequenas** (antes de `5490b99`): listar a pasta por vídeo tornava a varredura quadrática
  (1.000 clipes levavam 212 s e 3.000 não terminavam). Cada pasta agora é listada uma vez.

> **Correção de medição.** Uma versão anterior desta página dava ao Jellyfin 5,42× e depois 20,5× na transcodificação,
> porque o *harness* contava 6 s por segmento e o Jellyfin usa segmentos de 3 s: ele baixava 18 s de vídeo e era
> creditado com 36 s. O cálculo agora soma a duração real (`#EXTINF`) e os dois baixam a mesma quantidade de vídeo.

## Por que o Rustyfin vai melhor

- **Memória e imagem:** binário único em Rust, sem coleta de lixo e sem a pilha .NET.
- **API:** consultas simples ao SQLite, sem camada de serviços entre a rota e o banco. A diferença cresce com a
  concorrência.
- **Direct Play:** leitura em blocos de 256 KB sem copiar o arquivo para a memória.
- **Varredura:** cada pasta é listada uma única vez e o hash de conteúdo, que custa uma leitura inteira, só é calculado
  quando decide algo.

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
  trabalho. Cada sessão baixa 36 s de vídeo (a duração real dos segmentos, somada pelo `#EXTINF`: 6 s no Rustyfin e 3 s no Jellyfin); a "velocidade" é esses 36 s divididos pelo tempo total. O Jellyfin usa
  o `jellyfin-ffmpeg` próprio dele; o Rustyfin usa o `ffmpeg` da imagem Debian.
- **Estabilidade:** 5 minutos de carga mista (2 leitores de arquivo, navegação pela API e uma transcodificação em
  repetição). A memória é medida **em repouso, antes e depois**, porque durante a carga o `docker stats` soma os
  processos do `ffmpeg`.
- **Varredura:** o tempo conta desde o pedido de varredura até o número esperado de itens aparecer na API (e até a
  tarefa terminar).

## Ressalvas

- **Uma máquina, uma rodada completa.** Há variação entre execuções: em outras rodadas o Direct Play do Rustyfin ficou
  entre 132 e 195 MB/s e o do Jellyfin entre 57 e 95 MB/s.
- **Rodadas diferentes por sistema.** O Rustyfin foi medido por inteiro na v0.17.4 (só o reinício a quente foi refeito na v0.17.5, depois da correção do `SIGTERM`); o Jellyfin, que não mudou, foi medido
  no mesmo dia em rodadas anteriores (e o cenário de transcodificação dele foi refeito depois da correção de medição).
- **Perfil restrito:** não mede escala (1.000 e 3.000 clipes), transcodificação com 2 e 4 sessões nem estabilidade. Com
  1 CPU e 512 MB a transcodificação encosta no limite de memória nos dois.
- A "RAM" é a do cgroup do container (`docker stats`), que inclui processos filhos como o `ffmpeg`.
- O "reinício a quente" é o `docker restart` completo até a API responder, ou seja, inclui o tempo de o Docker encerrar o processo.
- O Jellyfin não terminou a varredura de 3.000 clipes em 15 minutos (limite do teste); o número real é maior.
- O Jellyfin usa o `jellyfin-ffmpeg` próprio e segmentos de 3 s; o Rustyfin usa o `ffmpeg` da imagem Debian e segmentos
  de 6 s. A velocidade é medida sobre a mesma quantidade de vídeo, não sobre o mesmo número de segmentos.
- **Aceleração por hardware** não entra neste comparativo (o Docker Desktop não repassa a GPU); as medidas numa Intel real
  estão em [reprodução](reproducao.md#aceleração-por-hardware).
- **Não medido:** legendas, HDR, ABR comparável (o Jellyfin tem uma qualidade só nesse perfil), TMDB, bibliotecas com
  muitos metadados e os clientes reais (Moonfin, Swiftfin, Findroid).

## Resultados completos

<details>
<summary>Tabelas dos perfis folgado e restrito</summary>

### Perfil folgado

| Métrica | Rustyfin | Jellyfin | Vantagem do Rustyfin |
|---|---|---|---|
| Imagem Docker (MB) | 791 | 2 490 | 3,1× |
| Subida até responder (s, volume novo) | 0,6 | 6,0 | 10,8× |
| Reinício a quente (s) | 0,8 | 2,6 | 3,4× |
| RAM em repouso, biblioteca vazia (MiB) | 5 | 191 | 36,4× |
| RAM em repouso, biblioteca indexada (MiB) | 11 | 391 | 34,8× |
| CPU em repouso, indexada (%) | 0,0 | 0,5 |  |

**Biblioteca**

| Métrica | Rustyfin | Jellyfin | Vantagem |
|---|---|---|---|
| 1ª varredura de 18 vídeos reais: visíveis na API (s) | 1,0 | 38,5 | 39,6× |
| 1ª varredura: concluída (s) | 3,0 | 57,7 | 19,1× |
| Pico de RAM na varredura (MiB) | 203 | 462 | 2,3× |
| Rescan sem mudanças (s) | 0,14 | 6,07 | 42,4× |
| Varredura de 1000 clipes (s) | 14,7 | 412,7 | 28,1× |
|   pico de RAM com 1000 clipes (MiB) | 57 | 289 | 5,1× |
|   listar 50 itens com 1000 clipes (ms, p50) | 3,5 | 31,0 | 8,8× |
| Varredura de 3000 clipes (s) | 45,7 | não terminou no limite do teste |  |
|   pico de RAM com 3000 clipes (MiB) | 70 | 352 | 5,0× |
|   listar 50 itens com 3000 clipes (ms, p50) | 7,1 | — |  |
| Arquivo novo aparece (s, com 1000 clipes) | 0,8 | 3,9 | 5,0× |

**API (p50 em ms e requisições por segundo; mediana de 3 rodadas)**

| Rota | Conexões | Rustyfin p50 | Jellyfin p50 | Rustyfin req/s | Jellyfin req/s |
|---|---|---|---|---|---|
| `UserViews` | 1 | 1,5 | 17,1 | 316 | 45 |
| `Items?Recursive` | 1 | 2,1 | 29,3 | 232 | 38 |
| `Items/{id}` | 1 | 1,4 | 31,8 | 284 | 30 |
| `Latest` | 1 | 2,0 | 17,1 | 212 | 45 |
| `Shows/Seasons` | 1 | 1,6 | 12,1 | 277 | 56 |
| `UserViews` | 10 | 3,2 | 46,1 | 2 828 | 193 |
| `Items?Recursive` | 10 | 5,0 | 55,1 | 1 480 | 171 |
| `Items/{id}` | 10 | 3,1 | 114,2 | 2 472 | 81 |
| `Latest` | 10 | 5,7 | 46,0 | 1 202 | 208 |
| `Shows/Seasons` | 10 | 3,2 | 35,0 | 2 475 | 257 |
| `UserViews` | 50 | 7,0 | 242,2 | 2 741 | 196 |
| `Items?Recursive` | 50 | 28,8 | 293,0 | 1 120 | 164 |
| `Items/{id}` | 50 | 6,5 | 840,6 | 2 353 | 54 |
| `Latest` | 50 | 58,3 | 325,7 | 790 | 138 |
| `Shows/Seasons` | 50 | 7,7 | 232,6 | 2 344 | 198 |

**Direct Play**

| Métrica | Rustyfin | Jellyfin | Vantagem |
|---|---|---|---|
| Tempo até o 1º byte (ms) | 4,8 | 13,7 | 2,9× |
| Vazão de 1 stream (MB/s) | 182 | 78 | 2,3× |
| Seek de 64 KB, p50 (ms) | 4,0 | 9,2 | 2,3× |
| Seek de 64 KB, p95 (ms) | 24,9 | 28,6 | 1,2× |
| 5 streams simultâneos, vazão agregada (MB/s) | 261 | 200 | 1,3× |
| 10 streams simultâneos, vazão agregada (MB/s) | 254 | 205 | 1,2× |
| 20 streams simultâneos, vazão agregada (MB/s) | 224 | 206 | 1,1× |

**Transcodificação HLS (H.264, uma qualidade; 36 s de vídeo)**

| Sessões | Sistema | OK | 1º segmento (s) | Velocidade por sessão (× tempo real) | CPU máx (%) | RAM máx (MiB) |
|---|---|---|---|---|---|---|
| 1 | Rustyfin | 1/1 | 0,6 | 13,98 | 363 | 256 |
| 1 | Jellyfin | 1/1 | 0,7 | 12,17 | 389 | 665 |
| 2 | Rustyfin | 2/2 | 1,7 | 4,63 | 398 | 486 |
| 2 | Jellyfin | 2/2 | 1,9 | 3,24 | 405 | 952 |
| 4 | Rustyfin | 4/4 | 5,1 | 1,27 | 406 | 950 |
| 4 | Jellyfin | 4/4 | 4,9 | 1,27 | 404 | 1 507 |
| 1 (ABR, 2 qualidades) | Rustyfin | 1/1 | 0,7 | 14,28 | 336 | 257 |

**Estabilidade (Rustyfin, 5 min de carga mista)**: RAM em repouso 28 → 27 MiB (pico durante a carga 266), 1378 requisições de API, 0 erros.

**Estabilidade (Jellyfin, 5 min de carga mista)**: RAM em repouso 398 → 392 MiB (pico durante a carga 1 666), 450 requisições de API, 0 erros.

### Perfil restrito

| Métrica | Rustyfin | Jellyfin | Vantagem do Rustyfin |
|---|---|---|---|
| Imagem Docker (MB) | 791 | 2 490 | 3,1× |
| Subida até responder (s, volume novo) | 0,5 | 3,9 | 7,6× |
| Reinício a quente (s) | 0,6 | 6,2 | 10,1× |
| RAM em repouso, biblioteca vazia (MiB) | 5 | 180 | 37,7× |
| RAM em repouso, biblioteca indexada (MiB) | 9 | 374 | 41,1× |
| CPU em repouso, indexada (%) | 0,0 | 0,4 |  |

**Biblioteca**

| Métrica | Rustyfin | Jellyfin | Vantagem |
|---|---|---|---|
| 1ª varredura de 18 vídeos reais: visíveis na API (s) | 2,7 | 37,8 | 13,9× |
| 1ª varredura: concluída (s) | 3,0 | 56,0 | 18,5× |
| Pico de RAM na varredura (MiB) | 121 | 444 | 3,7× |
| Rescan sem mudanças (s) | 0,11 | 7,53 | 65,7× |

**API (p50 em ms e requisições por segundo; mediana de 3 rodadas)**

| Rota | Conexões | Rustyfin p50 | Jellyfin p50 | Rustyfin req/s | Jellyfin req/s |
|---|---|---|---|---|---|
| `UserViews` | 1 | 1,5 | 17,1 | 223 | 44 |
| `Items?Recursive` | 1 | 2,1 | 29,1 | 181 | 38 |
| `Items/{id}` | 1 | 1,4 | 38,8 | 220 | 28 |
| `Latest` | 1 | 2,0 | 29,1 | 187 | 41 |
| `Shows/Seasons` | 1 | 1,5 | 17,6 | 222 | 47 |
| `UserViews` | 10 | 3,2 | 135,3 | 1 928 | 70 |
| `Items?Recursive` | 10 | 6,6 | 151,2 | 753 | 63 |
| `Items/{id}` | 10 | 3,2 | 211,0 | 2 055 | 45 |
| `Latest` | 10 | 6,6 | 121,4 | 614 | 77 |
| `Shows/Seasons` | 10 | 3,5 | 100,2 | 1 398 | 97 |
| `UserViews` | 50 | 13,1 | 678,6 | 2 139 | 73 |
| `Items?Recursive` | 50 | 68,4 | 749,9 | 767 | 67 |
| `Items/{id}` | 50 | 10,6 | 997,7 | 2 175 | 49 |
| `Latest` | 50 | 85,3 | 609,3 | 604 | 81 |
| `Shows/Seasons` | 50 | 15,6 | 478,9 | 1 252 | 103 |

**Direct Play**

| Métrica | Rustyfin | Jellyfin | Vantagem |
|---|---|---|---|
| Tempo até o 1º byte (ms) | 11,8 | 12,1 | 1,0× |
| Vazão de 1 stream (MB/s) | 182 | 91 | 2,0× |
| Seek de 64 KB, p50 (ms) | 4,1 | 9,6 | 2,3× |
| Seek de 64 KB, p95 (ms) | 27,5 | 30,7 | 1,1× |
| 5 streams simultâneos, vazão agregada (MB/s) | 255 | 206 | 1,2× |
| 10 streams simultâneos, vazão agregada (MB/s) | 260 | 202 | 1,3× |
| 20 streams simultâneos, vazão agregada (MB/s) | 221 | 194 | 1,1× |

**Transcodificação HLS (H.264, uma qualidade; 36 s de vídeo)**

| Sessões | Sistema | OK | 1º segmento (s) | Velocidade por sessão (× tempo real) | CPU máx (%) | RAM máx (MiB) |
|---|---|---|---|---|---|---|
| 1 | Rustyfin | 1/1 | 2,1 | 3,31 | 100 | 253 |
| 1 | Jellyfin | 1/1 | 2,2 | 2,60 | 100 | 510 |
| 1 (ABR, 2 qualidades) | Rustyfin | 1/1 | 1,9 | 3,53 | 100 | 254 |

</details>
