# Changelog

Formato baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/);
versionamento [SemVer](https://semver.org/lang/pt-BR/) (na série 0.x, mudanças incompatíveis sobem o MINOR).

## [Não lançado]

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
