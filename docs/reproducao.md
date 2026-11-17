# Reprodução

Quando um app vai tocar um item, ele envia ao servidor o seu `DeviceProfile` (contêineres, codecs e limite de bitrate
que o aparelho aceita) em `POST /Items/{id}/PlaybackInfo`. O Rustyfin compara o perfil com o arquivo e escolhe a forma
de entrega menos custosa.

## As três formas

| Forma | Quando | Custo |
|---|---|---|
| **Direct Play** | O aparelho aceita o contêiner, o vídeo e o áudio como estão e o bitrate cabe no limite | Nenhum: o arquivo é entregue com HTTP `Range` |
| **Remux** | O vídeo é aceito, mas o áudio ou o contêiner não (ex.: HEVC + DTS) | Baixo: o vídeo é **copiado** e só o áudio/contêiner mudam |
| **Transcode** | O codec de vídeo não é aceito ou o bitrate do arquivo passa do limite do cliente | Alto: o vídeo é recodificado para H.264 |

Sem `ffmpeg`, o servidor continua só em Direct Play. O limite de transcodificações simultâneas é
`RUSTYFIN_MAX_TRANSCODES` (as excedentes recebem 503).

## Direct Play

- Responde `Range` com `206`, trata faixas inválidas com `416` e informa `Accept-Ranges`, `Content-Range`,
  `Last-Modified` e `ETag`. A leitura é em blocos de 256 KB, sem carregar o arquivo na memória.
- Os arquivos MP4, M4V e MOV são informados ao cliente como contêiner `mov`, como o Jellyfin faz. O player da TV Samsung
  só lê esses arquivos por trechos (índice no fim do arquivo, várias conexões) quando a URL termina em `.mov`.

## HLS

Quando o perfil do cliente pede HLS, a entrega é por `master.m3u8` → `main.m3u8` → segmentos de 6 segundos.

- **MPEG-TS** por padrão; **fMP4** quando o perfil pede MP4 com protocolo HLS.
- **Transcode completo:** a playlist é **VOD**, com todos os segmentos listados desde o início, e os segmentos são gerados
  sob demanda. Ao avançar ou voltar para um ponto que o `ffmpeg` não vai alcançar, ele é reiniciado exatamente no
  segmento pedido. Uma sessão ociosa que foi encerrada é recriada ao pedir um segmento.
- **Remux:** como os cortes dependem dos quadros-chave do arquivo, a playlist cresce conforme o `ffmpeg` produz, e o
  avanço fica limitado ao trecho já gerado.
- **ABR:** quando o HLS recodifica um vídeo com 360 pixels de altura ou mais e há 2 Mbps ou mais para o vídeo, o player
  recebe duas qualidades (original e metade da altura) para adaptar o bitrate. As duas saem de um único processo.
- Sessões HLS ociosas são encerradas após 90 segundos e apagadas.

## Stream progressivo

Perfis que não usam HLS recebem um stream progressivo (MKV, MP4 fragmentado ou TS) por pipe. O processo morre junto com
a conexão do cliente.

## Legendas

| Origem | Como é entregue |
|---|---|
| Arquivo `.srt` ao lado do vídeo | Faixa externa, detectada na varredura |
| Texto embutido (SRT, ASS, mov_text, WebVTT) | Extraída pelo `ffmpeg` sob demanda como SRT, VTT ou ASS e guardada em cache; anunciada como `External` |
| Imagem embutida (PGS, DVD) | **Queimada no vídeo** quando selecionada, o que força transcode |

A rota de legendas é `/Videos/{id}/{fonte}/Subtitles/{n}/Stream.{srt|vtt|ass}`.
Faixas forçadas e o título de cada faixa são informados aos apps.

### Corrigir uma faixa SRT

Em **Diagnóstico → Ajustar legenda SRT**, busque o título e selecione uma faixa SRT externa ou
embutida. Em **Atraso fixo**, use um valor positivo para atrasar e negativo para adiantar a faixa.
Em **Desvio progressivo**, informe quando duas falas começam na legenda original e no vídeo.
Confira a prévia nos dois pontos e salve.

O ajuste fica no SQLite por item e faixa, e vale para todos os usuários. Reabra a reprodução no app
para buscar a faixa atualizada. **Remover ajuste** volta a servir a temporização original. O Rustyfin
preserva o arquivo e ignora o ajuste se a assinatura da origem mudar. O editor aceita SRT UTF-8 de até
2 MiB. A prévia de vídeo depende do suporte do navegador ao arquivo original.

## HDR

Ao recodificar vídeo HDR PQ ou HLG, o `ffmpeg` (com os filtros `zscale` e `tonemap`) converte para SDR BT.709. Com vídeo
copiado, o HDR é mantido.

## Imagens

- Capas e fundos locais ou do TMDB; sem elas, um quadro do vídeo, gerado em segundo plano depois de cada varredura.
- `maxWidth`, `maxHeight` e `quality` redimensionam sob demanda, com cache em disco, `ETag` e resposta `304`.

## Estado de reprodução

Progresso, "assistido", favoritos e contagem de reproduções são gravados **por usuário** por
`Sessions/Playing`, `.../Progress` e `.../Stopped`. Um item é dado como assistido ao passar de 90% da duração.


## Áudio, cenas e versões preparadas

Em **Bibliotecas → Manutenção do acervo → Indexar áudio e cenas**, selecione um título. Indexe a
primeira faixa de áudio **antes de substituir a edição**. Na revisão, escolha **Sugerir pela trilha de áudio**. A comparação
é local, usa até três horas de áudio por edição e devolve somente correspondências distintas
com baixa distância entre as assinaturas. Permanece disponível a confirmação por perfil.

**Indexar cenas SRT** registra falas e tempos de um título. Em `/cenas`, entre com o seu perfil.
A busca limita as prévias às bibliotecas permitidas e aos trechos que terminam até a posição
assistida. Títulos marcados como assistidos têm todas as falas disponíveis. É possível liberar
spoilers explicitamente. Ao alternar esse filtro, a página refaz a busca preenchida. Reindexe
depois de trocar a legenda ou seu ajuste.

Em **Diagnóstico → Versões recomendadas**, o servidor recomenda MP4 progressivo após duas
sessões relatadas com a mesma configuração elegível. O preparo usa a faixa de áudio padrão,
H.264 e AAC. O cache padrão é de 2 GiB, ajustável com `RUSTYFIN_PREPARO_MAX_BYTES`.
Se uma sessão relatar atividade, o preparo interrompe a conversão e recomeça quando o servidor
estiver ocioso. O arquivo original é preservado. Recomendações e trabalhos em andamento ficam
em memória. A versão pronta é usada em novas reproduções compatíveis a partir do início.
