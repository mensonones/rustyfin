# 🗺️ Rustyfin - Roadmap de Desenvolvimento

> **Objetivo:** Criar um servidor de mídia ultraleve em Rust (baixo consumo de RAM/CPU, zero GC) rodando em `localhost`, com **100% de compatibilidade** com clientes do ecossistema Jellyfin (foco inicial: **Moonfin**, **Swiftfin**, **Findroid** e **Jellyfin Media Player**).

---

## 🎯 Filosofia do Projeto
- **Localhost First:** Sem complexidade desnecessária de certificados TLS, proxies reversos ou autenticação multi-tenant pesada no início.
- **Direct Play Prioritário:** Quase todos os clientes modernos (especialmente Moonfin via MPV/AVPlayer) suportam Direct Play nativo de MKV, HEVC, AV1 e legendas ASS. O servidor deve focar em **HTTP Range Requests** de altíssima velocidade.
- **Eficiência Extrema:** Consumo alvo de RAM em repouso < 30 MB e inicialização em milissegundos.
- **Single Binary:** Um único executável com banco SQLite embutido (modo WAL).

---

## 📅 Fases do Desenvolvimento

### Fase 1: Fundação & Handshake Jellyfin (Concluída / Em Andamento)
- [x] Estruturação do projeto Cargo e arquitetura modular.
- [x] Banco de dados SQLite com modo WAL (`sqlx` / `rusqlite`).
- [x] Rotas de Descoberta e Info do Servidor:
  - `GET /System/Info/Public`
  - `GET /System/Info`
  - `GET /System/Ping`
- [x] Rotas de Autenticação para Localhost:
  - `GET /Users/Public`
  - `POST /Users/AuthenticateByName` (login automático / sem senha no modo local)
  - `GET /Users/{id}`

### Fase 2: Scanner de Mídia & Indexação (Implementada)
- [x] Varredura de pastas configuradas (`walkdir` em `spawn_blocking`), incremental por tamanho/mtime e com remoção de arquivos ausentes.
- [x] Extração de metadados:
  - [x] Duração, resolução e codecs principais via `ffprobe` (sem ffprobe: campos ficam vazios e o item é reinspecionado no próximo scan).
  - [x] Todas as trilhas de áudio/legenda com canais, idioma, título e flags default/forçada (trilhas antigas são reinspecionadas uma vez).
  - [x] Limpeza de título, ano e tags de release; detecção de episódios (`S01E02`, `1x02`, pastas `Serie/Season 1`).
- [x] Indexação de bibliotecas no SQLite:
  - [x] UUIDs determinísticos (v5 do caminho).
  - Armazenamento de metadados (duração em ticks, resolução, canais de áudio, legendas).
- [x] Rotas de navegação da biblioteca (paginação/ordenação/filtros em SQL, `Items/Resume`, `Items/Latest`):
  - `GET /Users/{userId}/Views` (exposição de pastas: "Filmes", "Séries", "Animes")
  - `GET /Users/{userId}/Items` (listagem com paginação e ordenação)
  - `GET /Items/{id}` (detalhes de um item específico)

#### Melhorias da revisão do scanner

- [x] Reaproveitar a listagem de cada pasta durante a varredura ao localizar vídeos, capas e legendas. Isso evita repetir leituras em pastas com muitos arquivos e em montagens lentas.
- [x] Agrupar episódios com e sem ano da mesma série quando a associação for inequívoca. Manter separadas séries homônimas de anos distintos.

### Fase 2b: Autoscan & Metadados Online
- [x] Scan automático periódico (`RUSTYFIN_SCAN_INTERVAL_SECS`), por eventos de arquivo e sob demanda (`POST /Library/Refresh`, `/Items/{id}/Refresh`).
- [x] Provider TMDB opcional (`TMDB_API_KEY`): sinopse, nota, gêneros, ano, título localizado e posters/backdrops com cache em disco; episódios usam nome/sinopse/still do episódio.
- [x] **Hierarquia de séries**: itens virtuais `Series` e `Season` (IDs determinísticos), view "Séries" (`tvshows`), `ParentId` por série/temporada, `/Shows/{id}/Seasons`, `/Shows/{id}/Episodes` (`seasonId`/`season`), `/Shows/NextUp`, contagem de não assistidos e imagens da série/temporada.
- [x] Poster e backdrop locais da série (`poster.jpg`, `backdrop.jpg` ou `fanart.jpg`) e capa da temporada (`folder.jpg`), com fallback para TMDB.
- [x] Elenco (20 atores), diretores e roteiristas, estúdios, classificação indicativa (do país do idioma configurado, depois US), slogan e trailers do YouTube, via TMDB (`append_to_response`). Itens e séries já enriquecidos são completados em segundo plano. Fotos das pessoas (`PrimaryImageTag` e `/Items/{id}/Images/Primary`, baixadas do TMDB na primeira requisição e guardadas em cache, com `maxWidth` e ETag).
- [x] Scan por eventos do sistema de arquivos (`notify`, com debounce de 3 s), mantendo o polling como reserva (`RUSTYFIN_WATCH=false` desativa o watcher).

### Fase 2c: Compatibilidade com clientes 10.9+
- [x] `UserViews`, `Items/Latest`, `QuickConnect/Enabled`, `Branding/Css`, `Library/MediaFolders`, `System/Endpoint`, `Sessions`, `Playback/BitrateTest`, `Items/{id}/Download`, `PlaybackInfo` via GET.
- [x] Marcar assistido/não assistido e favoritos (filmes, episódios, séries e temporadas), `Filters=IsFavorite`.
- [x] Nomes de episódio em várias formas: `S01E02`, `1x02`, `Temporada 1 Episódio 2`, `Season 1 Episode 2`, `T1 E2`.
- [x] Respostas vazias válidas para `Genres`, `Studios`, `Persons`, `Similar`, `Intros`, `ThemeMedia`, `MediaSegments`, `Items/Filters`.
- [x] Validado no **Moonfin 3.0.0 (Android)**: conexão, login, home com destaques, bibliotecas Filmes/Séries (série → temporada → episódio, NextUp), Direct Play com seek, transcode HLS por limite de bitrate, marcar assistido, WebSocket `/socket`.
- Validação manual opcional: testar a compatibilidade com Swiftfin, Findroid e TV. `Moonfin/*` e `Plugins/*` retornam 404 de propósito (plugin do servidor não instalado).
- [x] Descoberta automática (UDP 7359, protocolo "who is JellyfinServer?"): validada no Moonfin. No Docker defina `RUSTYFIN_PUBLIC_URL` (IP do PC) no `.env`; o app esconde da lista "descobertos" os servidores já salvos.
- [x] `GenreItems`, `Studios`, `People`, `OfficialRating`, `Taglines` e `RemoteTrailers` nos itens; listas `/Genres`, `/Studios`, `/Persons` (com `PersonTypes`, busca e paginação) e `/Items/Filters`; filtros `Genres`, `Years` e `OfficialRatings` em `/Items` (filmes, séries e episódios).

### Fase 2d: Administração (Implementada)
- [x] Bibliotecas e configurações persistidas no banco (`libraries`, `library_paths`, `settings`), com migração e *seed* a partir do ambiente.
- [x] Rotas `/Library/VirtualFolders` (criar, listar, excluir, adicionar/remover pasta, renomear) compatíveis com o Jellyfin; uma biblioteca por pasta e item atribuído à mais específica.
- [x] Painel web `/admin` embutido no binário: bibliotecas, seletor de pastas, TMDB, status e scan.
- [x] Acesso só de rede privada/loopback; senha opcional (`RUSTYFIN_ADMIN_PASSWORD`).
- [x] **Usuários e senhas**: Argon2, tokens por dispositivo validados em toda rota (exceto rotas públicas, como login e avatares), limite de tentativas, progresso e favoritos por usuário, acesso por biblioteca, administradores, painel com login.
- [x] Revisão de segurança/correção (Argus, v0.2.0): limpeza do scan, login sem senha só na LAN, logs sem token, trava do último admin e outros.
- [x] Autenticar imagens e segmentos HLS com autorização por usuário e biblioteca, mantendo a compatibilidade com os apps e o carregamento de avatares na tela de login.
- [x] Aba **Sessões** no painel: dispositivos conectados (aparelho, app, usuário, último uso), encerrar uma ou todas; expiração de tokens parados (`RUSTYFIN_TOKEN_TTL_DAYS`, desligada por padrão).
- [x] Perfil oculto (fora da lista da tela de login; entra digitando o nome) e foto de perfil (`GET/POST/DELETE /Users/{id}/Images/Primary`, JPEG/PNG/WebP/GIF até 8 MB, enviada pelo app ou pelo painel).
- [x] Painel: nome do servidor, intervalo do scan e limite de transcodificações simultâneas, aplicados na hora, sem reiniciar (têm prioridade sobre as variáveis de ambiente).

### Fase 3: Motor de Streaming Direto (Direct Play & Scrubbing) (Implementada)
- [x] Implementação de **HTTP 206 Partial Content** com suporte completo a headers `Range` e `Content-Range`.
- [x] Suporte a seeking instantâneo (inclui 416 para ranges inválidos) (avanço e retrocesso rápido no player).
- [x] Rotas de streaming:
  - `GET /Videos/{id}/stream`
  - `GET /Videos/{id}/stream.{container}`
  - `GET /Audio/{id}/stream`
- [x] Endpoint de compatibilidade de reprodução (ainda sem DeviceProfile):
  - `POST /Items/{id}/PlaybackInfo` (informa ao Moonfin que o arquivo suporta Direct Play)

### Fase 4: Sincronização de Estado & Sessão (Scrobbling) (Implementada)
- [x] Rastreamento de progresso de visualização:
  - `POST /Sessions/Playing`
  - `POST /Sessions/Playing/Progress`
  - `POST /Sessions/Playing/Stopped`
- [x] Persistência de "Continuar Assistindo" (*Resume playback*) e marcação de assistido (*Played status*).

### Fase 5: Capas & Imagens (Artwork) (Implementada)
- [x] Suporte a posters/backdrops locais: `<nome>.jpg`, `<nome>-poster.jpg`, `<nome>-fanart.jpg` e, se o vídeo está sozinho na pasta, `poster/cover/folder` e `fanart/backdrop`.
- [x] Extração de thumbnail sob demanda via ffmpeg (cache em `RUSTYFIN_CACHE_DIR`, máx. 2 processos simultâneos, timeout 30s)  do arquivo de vídeo caso não haja imagem local.
- [x] Rotas de imagens (sem redimensionamento: `maxWidth` e afins são ignorados):
  - `GET /Items/{id}/Images/Primary`
  - `GET /Items/{id}/Images/Backdrop`

### Fase 6: Transcodificação & Remuxing Sob Demanda (Implementada)
- [x] Decisão Direct Play / Remux / Transcode a partir do `DeviceProfile` (contêiner, codecs, `MaxStreamingBitrate`, `EnableDirectPlay/Transcoding`) e do bitrate médio do arquivo.
- [x] Smart Remuxing: copia o vídeo (`-c:v copy`) e só recodifica o áudio (AAC estéreo) quando o vídeo é compatível.
- [x] Saída HLS (`master.m3u8`/`main.m3u8`/segmentos `.ts`) e progressiva (mkv/mp4 fragmentado/ts); `StartTimeTicks` e faixa de áudio escolhida.
- [x] Processos isolados via `tokio::process`: limite simultâneo (`RUSTYFIN_MAX_TRANSCODES`), `kill_on_drop`, sessões HLS ociosas encerradas e apagadas, limpeza na inicialização.
- [x] Encoder configurável (`libx264` ou hardware via `RUSTYFIN_VIDEO_ENCODER`).
- [x] Validado no Docker com `ffmpeg`/`ffprobe` reais: HLS H.264+AAC, remux HEVC preservado, `StartTimeTicks`, miniaturas, Direct Play com Range e sem processos órfãos.
- [x] Validado no Moonfin 3.0.0 em um aparelho Android físico: seleção e exibição de SRT embutido e externo em Direct Play, e SRT externo durante transcode HLS ao limitar a taxa a 2 Mbps.
- [x] *Seek* livre em HLS: na transcodificação completa a playlist é VOD (todos os segmentos de 6 s, com `ENDLIST`) e os segmentos são gerados sob demanda; ao avançar ou voltar além do que o ffmpeg vai gerar, ele é reiniciado no segmento pedido (`-ss`, `-start_number` e `-output_ts_offset` mantêm a linha do tempo). Validado no Docker com TS e fMP4, com e sem ABR. Com vídeo copiado (*remux*) os cortes dependem dos quadros-chave e a playlist continua `EVENT`.
- Validação manual opcional: testar HLS no Swiftfin e em outros clientes, incluindo o avanço no vídeo em uma TV Samsung.
- [ ] Desempenho (medido em [docs/desempenho.md](docs/desempenho.md)): a transcodificação por sessão ainda é mais lenta que a do Jellyfin com 4 CPUs (13,4× contra 20,5×); o reinício do container leva 3,8 s. (O hash de arquivo inteiro na varredura foi corrigido na v0.17.3 e as sessões HLS simultâneas que falhavam, na v0.17.4.)
- [x] Legendas: as de texto (SRT, ASS, mov_text, WebVTT) são extraídas pelo ffmpeg para `/Videos/{id}/{src}/Subtitles/{n}/[{ticks}/]Stream.{srt|vtt|ass}` (com cache) e anunciadas como `External` no `PlaybackInfo`; as de imagem (PGS/DVD) são queimadas no vídeo (transcode com *overlay*, ainda sem teste com arquivo real).
- [x] Legendas externas ao lado do vídeo (`Filme.srt` para `Filme.mkv`): detectadas na varredura e anunciadas como `External` nos detalhes do item e no `PlaybackInfo`.
- [x] HLS em fMP4 quando o perfil do cliente solicita MP4 com protocolo HLS. Perfis HLS sem essa opção usam segmentos MPEG-TS.
- [x] Conversão HDR PQ/HLG para SDR BT.709 ao recodificar vídeo com FFmpeg que inclui `zscale` e `tonemap`. Direct Play e remux conservam o HDR.
- [x] ABR no HLS em MPEG-TS e fMP4: duas qualidades com segmentos alinhados por sessão, ao recodificar vídeo a partir de 360p e com pelo menos 2 Mbps de orçamento para vídeo.

---

## Próximas funcionalidades

O produto tem foco exclusivo na reprodução local. O roadmap não inclui acesso remoto, relay
ou convites pela internet.

### Diagnóstico e confiabilidade

- [x] **Diagnóstico antes do Play.** Comparar até dois perfis recebidos de clientes locais
  para o mesmo título e faixas selecionadas. Mostrar Direct Play, remux ou transcodificação,
  os motivos da decisão e a última decisão observada por dispositivo.
- [x] **Identidade estável ao mover arquivos.** Preservar ID, progresso e favoritos quando
  o mesmo conteúdo reaparece em um único caminho novo. Cópias e correspondências ambíguas
  mantêm identidades separadas.
- [x] **Revisão de alterações do acervo.** Exibir substituições e possíveis edições no
  painel e permitir transferir favoritos e marcações de assistido para um destino escolhido.
- [x] **Correção de legendas SRT.** Salvar atraso e desvio de tempo por arquivo e faixa,
  com prévia em dois pontos e opção de restaurar o original.

- [x] **Retomar entre edições com dois pontos.** Mapear e confirmar a posição por perfil,
  com opção de indicar um tempo manualmente para cenas cortadas ou fora do intervalo.

### Pesquisa de produto

- [x] **Alinhamento automático entre edições.** Comparar assinaturas de áudio indexadas antes da troca.
  Usar a posição equivalente só quando houver confiança suficiente na correspondência.
- [x] **Preparar versões a partir das sessões.** Recomendar conversões MP4 progressivas que se repetem nos
  dispositivos locais, com orçamento de disco, pausa durante reprodução e originais preservados.
- [x] **Busca local de cenas com proteção contra spoilers.** Indexar SRT com timestamps e
  permitir que cada perfil filtre falas e prévias pelo ponto que já assistiu.

## 🧪 Como Testar com o Moonfin
1. Execute o servidor Rustyfin em `http://127.0.0.1:8096`.
2. Abra o Moonfin no iOS / macOS ou navegador.
3. No campo de endereço do servidor, digite `http://localhost:8096` quando o cliente estiver na mesma máquina.
4. Usuário padrão: `Admin` (sem senha).
