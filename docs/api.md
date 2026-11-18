# API

O Rustyfin implementa um subconjunto da API HTTP do Jellyfin 10.9 e, além dela, uma pequena API para o painel.
Os JSON usam **PascalCase** e as durações são em **ticks** (1 s = 10.000.000 ticks). IDs de itens são strings hexadecimais
de 32 caracteres (UUID sem hifens); os clientes podem enviar com hifens e maiúsculas, que o servidor normaliza.

## Autenticação

Toda rota exige um token, exceto as [públicas](usuarios-e-seguranca.md#o-que-não-exige-token). Formas aceitas:

- Cabeçalho `Authorization: MediaBrowser Client="...", Device="...", DeviceId="...", Version="...", Token="..."`
- Cabeçalhos `X-Emby-Token` ou `X-MediaBrowser-Token`
- Parâmetro de URL `api_key` ou `ApiKey`

O `userId` que algumas rotas trazem no caminho é ignorado: vale sempre o usuário do token.

## Rotas do Jellyfin

### Sistema e descoberta

| Rota | Observação |
|---|---|
| `GET /System/Info/Public`, `GET /System/Info`, `GET /System/Ping`, `GET /System/Endpoint` | Informações do servidor; as duas primeiras e o `Ping` são públicas |
| `GET /Branding/Configuration`, `GET /Branding/Css` | Públicas |
| `GET /QuickConnect/Enabled` | Sempre `false` |
| `GET /socket` | WebSocket aceito; as mensagens são ignoradas |
| `GET /Playback/BitrateTest` | Teste de largura de banda |
| `GET /System/Configuration/encoding` | Configuração de codificação (leitura) |

### Usuários

| Rota | Observação |
|---|---|
| `GET /Users/Public` | Perfis visíveis na tela de login (sem desativados nem ocultos) |
| `POST /Users/AuthenticateByName` | Login; devolve o token do dispositivo |
| `GET /Users`, `GET /Users/Me`, `GET /Users/{id}` | Dados do usuário |
| `POST /Users/New`, `DELETE /Users/{id}` | Só administradores |
| `POST /Users/{id}/Password` | Troca de senha |
| `POST /Users/{id}/Policy` | Política (`IsAdministrator`, `IsDisabled`, `IsHidden`, `EnableAllFolders`, `EnabledFolders`) |
| `GET/POST/DELETE /Users/{id}/Images/Primary` | Foto de perfil: o GET é público; POST e DELETE só o próprio usuário ou um administrador. Corpo: imagem ou base64 (JPEG, PNG, WebP, GIF, até 8 MB) |
| `POST /Sessions/Logout`, `GET /Sessions`, `POST /Sessions/Capabilities[/Full]` | Sessão do dispositivo |

### Biblioteca e itens

| Rota | Observação |
|---|---|
| `GET /UserViews`, `GET /Users/{id}/Views`, `GET /Library/MediaFolders` | Bibliotecas do usuário |
| `GET /Items`, `GET /Users/{id}/Items` | Listagem com paginação, ordenação e filtros: `ParentId`, `IncludeItemTypes`, `SearchTerm`, `Filters`, `IsPlayed`, `Genres`, `Years`, `OfficialRatings`, `SeasonId`... |
| `GET /Items/{id}`, `GET /Users/{id}/Items/{id}` | Detalhes de filme, episódio, série, temporada ou biblioteca |
| `GET /Items/Latest`, `GET /UserItems/Resume` | Adicionados recentemente e "continuar assistindo" |
| `GET /Items/Filters`, `GET /Items/Filters2` | Gêneros, classificações e anos disponíveis |
| `GET /Genres`, `GET /Studios`, `GET /Persons` | Listas reais, com `SearchTerm`, `StartIndex`, `Limit` e `PersonTypes` |
| `GET /Shows/{id}/Seasons`, `GET /Shows/{id}/Episodes`, `GET /Shows/NextUp` | Séries |
| `POST /Items/{id}/Refresh`, `POST /Library/Refresh` | Dispara uma varredura |
| `GET/POST/DELETE /Library/VirtualFolders...` | Bibliotecas e pastas (rede local; veja [segurança](usuarios-e-seguranca.md#painel-de-administração)) |

Listas que o Rustyfin não modela respondem **vazias**: `Items/{id}/Ancestors`, `LocalTrailers`, `SpecialFeatures`,
`Intros`, `Similar`, `ThemeMedia` e `MediaSegments/{id}`.

### Imagens (token obrigatório)

| Rota | Observação |
|---|---|
| `GET /Items/{id}/Images/Primary[/{index}]` | Capa. Também serve a foto de uma pessoa (`People[].Id`) |
| `GET /Items/{id}/Images/Backdrop[/{index}]` | Fundo |
| Parâmetros | `maxWidth`, `maxHeight`, `quality`; respostas com `ETag` e `304` |

### Reprodução

| Rota | Observação |
|---|---|
| `POST /Items/{id}/PlaybackInfo` | Recebe o `DeviceProfile` e devolve as fontes e a decisão (Direct Play, Remux ou Transcode) |
| `GET /Videos/{id}/stream[.{ext}]`, `GET /Audio/{id}/stream`, `GET /Items/{id}/Download` | Arquivo com `Range` (`206`/`416`) ou stream progressivo quando há `transcode=` |
| `GET /Videos/{id}/master.m3u8`, `main.m3u8`, `low.m3u8` | HLS. As URLs geradas pelas playlists incluem `api_key` |
| `GET /Videos/{id}/hls/{chave}/{arquivo}` | Segmentos e arquivo de inicialização. Exigem token e acesso à biblioteca do vídeo |
| `GET /Videos/{id}/{fonte}/Subtitles/{n}/[{ticks}/]Stream.{srt\|vtt\|ass}` | Legendas de texto extraídas |
| `POST /Sessions/Playing`, `.../Progress`, `.../Stopped`, `.../Ping` | Progresso e fim da reprodução |
| `POST/DELETE /UserPlayedItems/{id}`, `/Users/{id}/PlayedItems/{id}` | Assistido |
| `POST/DELETE /UserFavoriteItems/{id}`, `/Users/{id}/FavoriteItems/{id}` | Favorito |
| `GET /DisplayPreferences/{id}` | Preferências de exibição (padrão) |

## API do painel

Usada por `/admin`. O painel abre em Bibliotecas, onde mostra a última varredura e permite iniciar outra. A aba Diagnóstico reúne os totais do acervo e o estado dos serviços. Segue as mesmas regras de acesso do painel (rede local e, se houver, senha de administrador).

| Rota | Descrição |
|---|---|
| `GET /admin` | Página do painel; `/admin/favicon[-dark].png` e `/admin/logo[-dark].png` são os ícones (claro e escuro) |
| `GET /admin/api/status` | Contagens, `ffmpeg`, TMDB, descoberta, último scan e `scan.signaturesPending` (arquivos cujo hash ainda é calculado em segundo plano) |
| `POST /admin/api/acervo/audio/{id}` | Indexa a primeira faixa de áudio para futuras revisões |
| `POST /admin/api/cenas/{id}` | Indexa trechos das faixas SRT de um título |
| `GET /admin/api/preparos` | Lista conversões MP4 sugeridas e estado do preparo |
| `POST /admin/api/preparos/{chave}` | Solicita o preparo de uma versão recomendada |
| `GET /admin/api/acervo/revisoes` | Até 30 revisões pendentes e seus destinos catalogados |
| `POST /admin/api/acervo/revisoes/{id}/previa` | Recebe `itemId` e `mapa` opcional com `anteriorA`, `novoA`, `anteriorB` e `novoB` em ticks. Retorna as posições anterior, atual e sugerida por perfil |
| `POST /admin/api/acervo/revisoes/{id}` | `{"acao":"manterSeparados"}` ou `{"acao":"transferir","itemId":"..."}`. A transferência aceita `retomadas` com `perfilId`, `posicaoTicks` e `posicaoEsperadaTicks`. Retorna 409 se o destino mudou no catálogo ou se a posição mudou desde a prévia |
| `GET /admin/api/legendas/{id}` | Lista faixas SRT ajustáveis do título |
| `GET/PUT/DELETE /admin/api/legendas/{id}/{índice}` | Consulta, salva ou remove o ajuste da faixa. `PUT` recebe a `versao` do GET e o objeto `ajuste` |
| `POST /admin/api/legendas/{id}/{índice}/previa` | Recebe o mesmo corpo do PUT e devolve amostras sincronizadas e WebVTT |
| `GET /admin/api/legendas/{id}/video` | Vídeo original para prévia no admin, com suporte a Range |
| `GET /admin/api/reproducao/titulos?busca=...` | Até 20 filmes e episódios que correspondem ao título |
| `GET /admin/api/reproducao/perfis` | Perfis locais observados no `PlaybackInfo`, com a última decisão por dispositivo |
| `POST /admin/api/reproducao/simular` | Recebe `itemId`, até dois IDs em `perfis`, `audioIndex` e `legendaIndex` opcionais. Devolve o plano por perfil e as faixas do título |
| `POST /admin/api/scan` | Inicia uma varredura imediata |
| `GET /admin/api/browse?path=...` | Lista as subpastas de um caminho (seletor de pastas) |
| `GET/PUT /admin/api/settings` | Nome do servidor, intervalo do scan, transcodificações, chave e idioma do TMDB |
| `GET/POST /admin/api/folders`, `.../folders/paths`, `.../folders/name` | Bibliotecas e pastas |
| `GET/POST /admin/api/users`, `DELETE /admin/api/users/{id}` | Usuários |
| `POST /admin/api/users/{id}/password`, `/policy`, `/rename` | Senha, permissões (inclui `isHidden`) e nome |
| `POST/DELETE /admin/api/users/{id}/avatar` | Foto de perfil (corpo: bytes da imagem) |
| `GET /admin/api/sessions`, `DELETE /admin/api/sessions[/{id}]` | Dispositivos conectados; encerrar uma ou todas as sessões |

Erros de validação devolvem `400` com `{"Message": "..."}`.

## Ferramenta de verificação

`python tools/client_probe.py http://127.0.0.1:8096` percorre o fluxo de um app e marca `FALHA` em qualquer rota sem
resposta. Os testes de integração em `tests/` cobrem a autenticação, a lista de login e as sessões.


O diagnóstico usa os perfis enviados pelos clientes ao consultar `PlaybackInfo` com ID de dispositivo.
Retém até 32 perfis em memória, cada um com a decisão mais recente. Reiniciar o servidor limpa
esses dados. A simulação usa o mesmo cálculo de `PlaybackInfo` e não inicia o FFmpeg. O plano é
uma previsão baseada nas capacidades declaradas pelo cliente.


A unidade dos ajustes é milissegundos. Envie `{"modo":"atraso","ms":1500}` para atraso fixo.
Para desvio progressivo, use `modo: "doisPontos"` e os campos `originalAMs`, `corrigidoAMs`,
`originalBMs` e `corrigidoBMs`. Se a origem mudar desde a consulta, a escrita retorna HTTP 409.


`GET /api/cenas?q=termo` exige token e respeita as bibliotecas do perfil. Por padrão só retorna
trechos concluídos no histórico do perfil. `spoilers=true` desativa apenas esse filtro.
A prévia de retomada aceita `automatico: true` quando nenhum mapa manual é informado.
