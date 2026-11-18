# Changelog

Formato baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/);
versionamento [SemVer](https://semver.org/lang/pt-BR/) (na série 0.x, mudanças incompatíveis sobem o MINOR).

## [Não lançado]

## [0.17.5] - 2026-11-18

### Corrigido
- O servidor ignorava o `SIGTERM`: como PID 1 no container, o `docker stop` esperava o prazo inteiro e terminava com
  `SIGKILL` (código 137), o que deixava todo reinício lento (3,6 s com o prazo curto desta máquina, 10 s no padrão do
  Docker). Agora `SIGTERM` e `SIGINT` encerram as transcodificações, fecham o banco (checkpoint do WAL) e saem com
  código 0: o reinício a quente caiu de 3,6 s para 0,8 s.

## [0.17.4] - 2026-11-17

### Corrigido
- Parar um filme transcodificado e abrir outro em seguida podia devolver 503 por até 90 s, porque o ffmpeg do primeiro seguia ocupando a vaga. Agora `Sessions/Playing/Stopped` encerra o ffmpeg na hora, e com o limite cheio a sessão parada há mais de 12 s cede a vaga. No benchmark, as 4 sessões simultâneas passaram de 2 para 4 bem-sucedidas.

### Alterado
- O x264 usa os mesmos ajustes de busca de movimento do Jellyfin (`subme=0`, `me=hex`, `rc_lookahead=10`): a velocidade de uma sessão transcodificada subiu de 3,0x para 13,8x o tempo real em 4 CPUs (Jellyfin: 12,2x). O benchmark contava 6 s por segmento e o Jellyfin usa 3 s, o que inflava o número dele; agora soma a duração real.

## [0.17.3] - 2026-11-17

### Corrigido
- A primeira varredura de uma biblioteca grande lia todos os arquivos inteiros para calcular o hash de conteúdo (286 s para 30 GB). Agora o hash só é calculado na varredura quando decide algo (arquivo substituído ou possível destino de um arquivo que sumiu) e o restante é preenchido em segundo plano: a mesma varredura leva 3 s. Um arquivo movido antes de o hash existir não é movido sozinho e vai para a fila de revisão do acervo.

## [0.17.2] - 2026-11-17

### Alterado
- Painel com diretório compacto de bibliotecas e coluna de operações.
- Busca de cenas com falas em destaque e resultados organizados em uma linha do tempo.

## [0.17.1] - 2026-11-17

### Alterado
- Painel administrativo com navegação lateral e manutenção agrupada abaixo das bibliotecas.
- Busca de cenas com status de carregamento, horários em coluna e atualização ao alternar spoilers.

## [0.17.0] - 2026-11-17

### Adicionado
- Sugestão de retomada por assinatura de áudio, com confirmação manual e fallback.
- Recomendações e preparo local de MP4 para conversões progressivas recorrentes.
- Busca autenticada de falas SRT com prévias filtradas pelo progresso do perfil.

## [0.16.0] - 2026-11-17

### Adicionado
- Mapeamento manual de retomada entre edições na revisão do acervo, com dois pontos,
  prévia por perfil e confirmação de cada posição.

## [0.15.0] - 2026-11-17

### Adicionado
- Fila de revisão em Bibliotecas para substituições, cópias ambíguas e possíveis novas edições.
  O administrador pode transferir favoritos e marcações de assistido para um destino confirmado.

## [0.14.0] - 2026-11-17

### Adicionado
- Ajuste reversível de legendas SRT externas e embutidas por título e faixa: atraso fixo,
  desvio progressivo por dois pontos e prévia no painel.

## [0.13.0] - 2026-11-17

### Alterado
- Mover um arquivo de mídia para outro caminho pode preservar o ID, o progresso e os favoritos.
  O scanner compara o SHA-256 e reaproveita o ID quando há uma única origem ausente e um
  único destino novo. Cópias simultâneas continuam como itens separados.

## [0.12.0] - 2026-11-17

### Adicionado
- Diagnóstico local antes do Play no painel. Compara até dois dispositivos, simula faixas
  de áudio e legenda e explica por que o servidor escolheria Direct Play, remux ou transcodificação.

## [0.11.0] - 2026-11-17

### Corrigido
- A varredura listava a pasta inteira a cada vídeo (um `stat` por entrada), o que a tornava quadrática: com 1.000 vídeos numa pasta levava 3,5 minutos e com 3.000 não terminava. Agora cada pasta é lida uma única vez: 1.000 arquivos em ~16 s, 3.000 em ~76 s, e um arquivo novo aparece em menos de 1 s.

### Adicionado
- Logotipo do projeto, com fundo transparente e uma variante para temas escuros, no `README` (que troca sozinho conforme o tema), no painel (cabeçalho e ícone da aba, também transparentes e com variante escura) e em `docs/assets/`, com uma imagem de pré-visualização social.

### Alterado
- **Compatibilidade com clientes Jellyfin:** capas e fundos exigem token no cabeçalho ou na query (`api_key`). Segmentos e arquivos de inicialização HLS também exigem token. As playlists já incluem a credencial nas URLs para players que não reenviam cabeçalhos. Avatares exibidos na tela de login continuam públicos. Veja a [documentação de segurança](docs/usuarios-e-seguranca.md).
- Episódios com e sem ano são agrupados quando a pasta da série tem um único ano conhecido. Séries homônimas de anos diferentes mantêm IDs distintos.
- Painel administrativo responsivo, com estado da varredura em Bibliotecas, listas compactas e diagnóstico de serviços.
- Documentação reorganizada em `docs/`: configuração, usuários e segurança, reprodução, clientes, arquitetura, API, solução de problemas, desempenho (antes `BENCHMARK.md`) e desenvolvimento. O `README` virou uma apresentação com início rápido e índice.
- Compilar o projeto agora exige **Rust 1.85 ou mais novo** (edição 2024). A imagem Docker não é afetada.
- Organização interna: biblioteca (`lib.rs`) separada do binário, `transcode` dividido em módulos, testes de integração em `tests/`, Clippy sem avisos na integração contínua e `AGENTS.md` com as diretrizes para agentes. Sem mudança de comportamento.

## [0.10.0] - 2026-11-17

### Adicionado
- Foto de perfil: `GET/POST/DELETE /Users/{id}/Images/Primary` e `PrimaryImageTag` no usuário. Aceita JPEG, PNG, WebP e GIF (até 8 MB, imagem crua ou base64 como o Jellyfin Web), só o próprio usuário ou um administrador troca. A tela de login mostra as fotos, e o painel permite enviar e remover.
- Perfil oculto (`IsHidden`): some da lista da tela de login, mas continua entrando ao digitar o nome. Configurável no painel e pela política do usuário.

## [0.9.0] - 2026-11-17

### Adicionado
- Aba **Sessões** no painel `/admin`: lista os dispositivos conectados (aparelho, app, usuário e último uso) e permite encerrar uma sessão ou todas. O token nunca é exposto.
- Expiração de tokens sem uso (`RUSTYFIN_TOKEN_TTL_DAYS`; por padrão nunca expira).
- Configurações do servidor no painel: nome (apps e descoberta automática), intervalo do scan e limite de transcodificações simultâneas. Aplicam-se na hora e têm prioridade sobre as variáveis de ambiente.

## [0.8.0] - 2026-11-17

### Adicionado
- Fotos das pessoas do elenco e da equipe: `People[].PrimaryImageTag`, `ImageTags` em `/Persons` e `/Items/{id}/Images/Primary` para a pessoa (baixada do TMDB na primeira requisição, com cache, `maxWidth` e ETag). Itens enriquecidos antes são completados em segundo plano.

## [0.7.0] - 2026-11-17

### Adicionado
- Elenco, diretores, roteiristas, estúdios, classificação indicativa, slogan e trailers do TMDB nos filmes, séries e episódios (herdam da série). Itens já enriquecidos são completados em segundo plano.
- `/Genres`, `/Studios`, `/Persons` e `/Items/Filters` com dados reais, e os filtros `Genres`, `Years` e `OfficialRatings` em `/Items`.

## [0.6.0] - 2026-11-17

### Adicionado
- Avanço livre em HLS: transcodificações completas oferecem uma playlist VOD com todos os segmentos e geram os segmentos sob demanda, reiniciando o ffmpeg no ponto pedido quando o usuário avança ou volta além do que já foi gerado. A sessão ociosa encerrada é recriada ao pedir um segmento. Com vídeo copiado (*remux*) a playlist continua crescendo.

## [0.5.1] - 2026-11-17

### Corrigido
- Direct Play na TV Samsung (Moonfin Tizen): arquivos MP4/M4V/MOV passam a ser informados como contêiner `mov` e o `PlaybackInfo` não envia mais `DirectStreamUrl`, como o Jellyfin; a TV monta a própria URL `stream.mov` e lê por trechos, abrindo em poucos segundos e permitindo avançar.
- O servidor HTTP envia os nomes de cabeçalho em `Title-Case` (`Accept-Ranges`, `Content-Range`...), como o Kestrel do Jellyfin.
- As respostas de vídeo trazem `Last-Modified` e `ETag` e respeitam `If-Range`; o `MediaSource` informa o `Bitrate`.
- Perfis de cliente que listam `mp4` ou `mov` reconhecem ambos para Direct Play.

## [0.5.0] - 2026-11-17

### Adicionado
- Capas e backdrops locais de séries e capas de temporadas, com preferência sobre as imagens correspondentes do TMDB.

## [0.4.0] - 2026-11-17

### Adicionado
- ABR em HLS MPEG-TS e fMP4. Transcodificações elegíveis oferecem duas qualidades simultâneas na playlist mestre.

## [0.3.0] - 2026-11-17

### Adicionado
- Legendas: trilhas de texto embutidas são extraídas (SRT/VTT/ASS) e entregues como `External` em `/Videos/{id}/{src}/Subtitles/{n}/Stream.{ext}`; legendas de imagem (PGS/DVD) são queimadas no transcode quando selecionadas.
- Trilhas informam título e flag de legenda forçada.
- Arquivos SRT com o mesmo nome do vídeo (ex.: `Filme.mkv` e `Filme.srt`) aparecem como trilhas externas após a varredura.
- HLS em fMP4 para perfis de cliente que pedem MP4 com protocolo HLS.
- Tone mapping de HDR PQ/HLG para SDR BT.709 na recodificação de vídeo, quando o FFmpeg oferece os filtros necessários.

## [0.2.0] - 2026-11-17

### Segurança
- Conta sem senha (como o `Admin` inicial) só autentica a partir de rede privada; de IP público o login é recusado.
- O log de requisições registra só método e caminho: o `api_key` da URL não vai mais para os logs.

### Alterado
- **IDs de séries e temporadas passam a incluir o ano** (do nome do arquivo ou da pasta da série): "Ghosts (2019)" e "Ghosts (2021)" são séries distintas. Os dados do TMDB e o poster da temporada migram sozinhos; progresso e favoritos não são afetados. Apps que guardaram o ID antigo precisam navegar de novo a partir da biblioteca.
- Direct Play lê o arquivo em blocos de 256 KB (era 4 KB): de ~22 para ~183 MB/s.
- Varredura em paralelo (`RUSTYFIN_SCAN_CONCURRENCY`), imagens redimensionadas (`maxWidth`/`maxHeight`/`quality`) com cache em disco e ETag/304, metadados do TMDB em lotes paralelos e pré-geração das miniaturas após cada varredura.

### Corrigido
- Varredura incompleta (erro de leitura em subpasta) ou falha ao ler as bibliotecas não apagam mais o catálogo nem o progresso.
- "Continuar assistindo" em biblioteca de séries retorna episódios; NextUp lê só séries já assistidas.
- O plano de reprodução usa o codec da faixa de áudio selecionada.
- IDs de item com hífen/maiúsculas são aceitos nos relatórios de progresso.
- Miniaturas geradas são renovadas quando o vídeo é substituído.
- Sempre sobra um administrador ativo, mesmo com pedidos simultâneos; salvar a chave do TMDB em paralelo não deixa o valor em uso desatualizado.

## [0.1.0] - 2026-11-17

Primeira versão.

### Adicionado
- Servidor de mídia em Rust (binário único + SQLite WAL) compatível com a API de clientes Jellyfin (Moonfin, Swiftfin, Findroid).
- Varredura automática paralela das pastas, com ffprobe, monitoramento de arquivos e parser de nomes (filmes e séries).
- Metadados do TMDB (sinopse, nota, gêneros, posters, arte por temporada) em lotes paralelos.
- Direct Play com `Range`, remux e transcodificação HLS via ffmpeg.
- Imagens com redimensionamento (`maxWidth`/`maxHeight`/`quality`), cache em disco, ETag/304 e pré-geração de miniaturas.
- Usuários com senha (Argon2), permissões por biblioteca, limite de tentativas e painel web em `/admin`.
- Descoberta automática na rede (UDP 7359).
- Imagem Docker com ffmpeg incluído.
