# Changelog

Formato baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/);
versionamento [SemVer](https://semver.org/lang/pt-BR/) (na série 0.x, mudanças incompatíveis sobem o MINOR).

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
