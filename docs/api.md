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

### Imagens (públicas)

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
| `GET /Videos/{id}/master.m3u8`, `main.m3u8`, `low.m3u8` | HLS |
| `GET /Videos/{id}/hls/{chave}/{arquivo}` | Segmentos e arquivo de inicialização (públicos; a chave da sessão é aleatória) |
| `GET /Videos/{id}/{fonte}/Subtitles/{n}/[{ticks}/]Stream.{srt\|vtt\|ass}` | Legendas de texto extraídas |
| `POST /Sessions/Playing`, `.../Progress`, `.../Stopped`, `.../Ping` | Progresso e fim da reprodução |
| `POST/DELETE /UserPlayedItems/{id}`, `/Users/{id}/PlayedItems/{id}` | Assistido |
| `POST/DELETE /UserFavoriteItems/{id}`, `/Users/{id}/FavoriteItems/{id}` | Favorito |
| `GET /DisplayPreferences/{id}` | Preferências de exibição (padrão) |

## API do painel

Usada por `/admin`. Segue as mesmas regras de acesso do painel (rede local e, se houver, senha de administrador).

| Rota | Descrição |
|---|---|
| `GET /admin` | Página do painel; `/admin/favicon[-dark].png` e `/admin/logo[-dark].png` são os ícones (claro e escuro) |
| `GET /admin/api/status` | Contagens, `ffmpeg`, TMDB, descoberta e último scan |
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
