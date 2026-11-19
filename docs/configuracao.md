# Configuração

O Rustyfin se configura de duas formas, que se combinam:

1. **Variáveis de ambiente**, lidas na inicialização. No Docker ficam no `docker-compose.yml` e no `.env`.
2. **Configurações do painel** (`/admin`), gravadas no banco e aplicadas na hora, sem reiniciar.

Para as configurações que existem nos dois lugares, **o valor salvo no painel vale mais** que a variável de ambiente.

## Variáveis de ambiente

### Servidor

| Variável | Padrão | Descrição |
|---|---|---|
| `RUSTYFIN_HOST` | `0.0.0.0` | Endereço de escuta. Use `127.0.0.1` para aceitar só conexões locais |
| `RUSTYFIN_PORT` | `8096` | Porta HTTP |
| `RUSTYFIN_SERVER_NAME` | `Rustyfin Local` | Nome mostrado nos apps e na descoberta automática (também editável no painel) |
| `RUSTYFIN_SERVER_ID` | fixo | ID do servidor. Só mude se rodar mais de uma instância na mesma rede |
| `RUSTYFIN_PUBLIC_URL` | IP detectado | Endereço anunciado na descoberta. **Em Docker é necessário** (ex.: `http://192.168.1.10:8096`), porque o container não conhece o IP do PC |
| `RUSTYFIN_DISCOVERY` | `true` | Responde à descoberta automática dos apps (UDP 7359) |
| `RUST_LOG` | `rustyfin=info,tower_http=debug` | Nível dos logs |

### Dados e bibliotecas

| Variável | Padrão | Descrição |
|---|---|---|
| `RUSTYFIN_DATA_DIR` | vazio | Pasta única para o banco (`rustyfin.db`) e o cache (`cache/`), criada se não existir. Pensada para a [instalação direta](instalacao-nativa.md) e serviços. Sem ela, valem `./rustyfin.db` e `./cache` (a pasta de onde o servidor foi iniciado) |
| `DATABASE_URL` | `sqlite://rustyfin.db?mode=rwc` | Banco SQLite (modo WAL) |
| `RUSTYFIN_MEDIA_DIRS` | `/media`, Vídeos do usuário, `./media` | Pastas de mídia separadas por vírgula. **Só vale na primeira execução**, para criar as bibliotecas "Filmes" e "Séries"; depois o banco é a fonte da verdade |
| `RUSTYFIN_CACHE_DIR` | `./cache` | Miniaturas, imagens redimensionadas, legendas extraídas, fotos e sessões HLS |
| `RUSTYFIN_SCAN_INTERVAL_SECS` | `300` | Scan automático periódico (`0` desliga; `POST /Library/Refresh` sempre funciona). Editável no painel |
| `RUSTYFIN_SCAN_CONCURRENCY` | núcleos (2 a 8) | Arquivos analisados em paralelo (`ffprobe`) |
| `RUSTYFIN_WATCH` | `true` | Monitora as pastas por eventos do sistema de arquivos. O scan periódico continua como reserva, útil em unidades de rede |

### Reprodução

| Variável | Padrão | Descrição |
|---|---|---|
| `RUSTYFIN_FFMPEG` | `ffmpeg` | Executável do ffmpeg |
| `RUSTYFIN_MAX_TRANSCODES` | automático | Transcodificações simultâneas; as excedentes recebem 503. Sem valor (aqui e no painel) o servidor calcula pelo hardware: **4 com GPU**, senão **metade das CPUs, de 2 a 8** (respeita `--cpus` do Docker). Um valor salvo no painel vale sobre o do ambiente |
| `RUSTYFIN_VIDEO_ENCODER` | `auto` | Encoder de vídeo: `auto` (GPU Intel/AMD via VAAPI se houver, senão `libx264`), `libx264`, `h264_vaapi`, `h264_qsv` (experimental). Veja [aceleração por hardware](reproducao.md#aceleração-por-hardware) |
| `RUSTYFIN_PREPARO_MAX_BYTES` | `2147483648` (2 GiB) | Tamanho máximo do cache de [versões preparadas](reproducao.md#áudio-e-versões-preparadas) (MP4 progressivo gerado a pedido do administrador). `0` impede novos preparos |
| `RUSTYFIN_HWACCEL_DEVICE` | `/dev/dri/renderD128` | Dispositivo de renderização da GPU para `h264_vaapi` e `h264_qsv` (Linux) |
| `RUSTYFIN_HWACCEL_DECODE` | `true` | A GPU também decodifica H.264 e HEVC. `false` deixa a decodificação na CPU |

### Acesso

| Variável | Padrão | Descrição |
|---|---|---|
| `RUSTYFIN_ADMIN_PASSWORD` | vazio | Senha extra do painel (HTTP Basic, qualquer usuário). Útil antes de criar um administrador com senha |
| `RUSTYFIN_TRUSTED_PROXIES` | vazio | IPs ou redes (separados por vírgula) dos proxies reversos em que o servidor confia para saber o IP real do cliente (`192.168.1.10` ou `172.28.0.10`, o IP exato do proxy, não a rede toda). Sem isso, requisições com `X-Forwarded-For` são tratadas como da internet. Veja [proxy reverso](proxy-reverso.md) |
| `RUSTYFIN_QUICK_CONNECT` | `true` | `0` desliga o [Quick Connect](usuarios-e-seguranca.md#quick-connect) (entrar na TV por um código) |
| `RUSTYFIN_IMAGENS_ANONIMAS` | `true` | Capas e fundos abertos sem token para clientes da rede local (necessário para o Moonfin na TV Samsung). `false` exige token nas imagens e as capas somem nos apps que não o enviam. Veja [usuários e segurança](usuarios-e-seguranca.md) |
| `RUSTYFIN_TOKEN_TTL_DAYS` | `0` (nunca) | Dias sem uso para um dispositivo perder o login e voltar à tela de entrada |

### Metadados (TMDB)

| Variável | Padrão | Descrição |
|---|---|---|
| `TMDB_API_KEY` | vazio | Chave v3 ou token v4 do TMDB. Sem ela, nenhum metadado online é buscado. Também editável no painel |
| `RUSTYFIN_TMDB_LANGUAGE` | `pt-BR` | Idioma dos metadados. A classificação indicativa usa o país do idioma (`pt-BR` → Brasil) e cai para os EUA |
| `RUSTYFIN_TMDB_BASE_URL` | `https://api.themoviedb.org/3` | Endereço da API (útil em testes) |
| `RUSTYFIN_TMDB_IMAGE_BASE` | `https://image.tmdb.org/t/p` | Endereço das imagens |
| `RUSTYFIN_ANILIST` | ligado | `0` desliga a busca de metadados de anime no AniList (só as bibliotecas do tipo Anime a usam; veja [anime](anime.md)) |
| `RUSTYFIN_ANILIST_URL` | `https://graphql.anilist.co` | Endereço da API do AniList (útil em testes) |

O servidor consulta o TMDB com o `curl`, que precisa estar no `PATH` (a imagem Docker já o inclui).

## Configurações do painel

Em **Configurações**:

- **Servidor:** nome, intervalo do scan automático e limite de transcodificações simultâneas.
- **TMDB:** chave de API e idioma.

Valores inválidos são recusados com uma mensagem, sem gravar nada.

## Docker

```yaml
services:
  rustyfin:
    build: .
    ports:
      - "8096:8096"        # HTTP
      - "7359:7359/udp"    # descoberta automática
    volumes:
      - ./data:/data       # banco SQLite e cache
      - ./media:/media     # suas mídias
```

O arquivo `docker-compose.override.yml` (ignorado pelo git) é o lugar para montar pastas do seu PC sem expor caminhos
pessoais no repositório.

### Usando pastas do seu PC no Docker

O container só enxerga o que for **montado como volume**; por isso o seletor de pastas do painel mostra o sistema de
arquivos *do container* (`/media`, `/mnt/...`). Isso é igual ao Jellyfin em Docker. Para usar uma pasta do Windows:

1. No `docker-compose.override.yml`, em `volumes`, adicione (somente leitura):
   `- "C:/Users/SEU_USUARIO/Videos:/mnt/videos:ro"`
2. Rode `docker compose up -d` e, em `/admin` → Bibliotecas, escolha `/mnt/videos` (ou uma subpasta).

Dica: monte uma pasta "pai" (por exemplo `C:/Users/SEU_USUARIO/Downloads:/mnt/downloads:ro`) para poder escolher
qualquer subpasta dela sem editar o compose de novo.

## Bibliotecas

- Cada biblioteca é do tipo **Filmes** ou **Séries** e tem uma ou mais pastas. Criar, renomear e mexer nas pastas
  dispara uma varredura imediata.
- Um arquivo pertence à biblioteca da **pasta mais específica** que o contém. Excluir uma biblioteca não apaga arquivos.
- As mesmas operações existem na API do Jellyfin (`POST/DELETE /Library/VirtualFolders`, `.../Paths`, `.../Name`) e em
  `POST /Library/Refresh`.

## Nomes de arquivo e pastas

| Padrão | Exemplo |
|---|---|
| Filme com ano | `Filme (2020).mkv`, `The.Matrix.1999.1080p.BluRay.mkv` |
| Episódio | `Serie S01E02.mkv`, `Serie 1x04.mkv`, `Serie Temporada 1 Episódio 2.mkv` |
| Por pastas | `Serie/Season 1/S01E02.mkv` |

Os termos de release (`1080p`, `BluRay`, `x264`...) são ignorados. O ano, quando presente no nome do arquivo ou da pasta
da série, separa séries de mesmo nome: `Ghosts (2019)` e `Ghosts (2021)` são séries diferentes.

### Imagens locais

| Para | Arquivo |
|---|---|
| Filme | `<nome>.jpg`, `<nome>-poster.jpg`, `<nome>-fanart.jpg` ou, se o filme está sozinho na pasta, `poster.jpg`/`cover.jpg`/`folder.jpg` e `fanart.jpg`/`backdrop.jpg` |
| Série | `poster.jpg` e `backdrop.jpg` (ou `fanart.jpg`) na pasta da série |
| Temporada | `folder.jpg` na pasta da temporada (`Serie/Season 1/folder.jpg`) |

Sem imagem local nem do TMDB, o servidor gera um quadro do próprio vídeo.

### Legendas

Arquivos `.srt` com o mesmo nome do vídeo (`Filme.srt` ao lado de `Filme.mkv`) são detectados na varredura.
Veja também [reprodução](reproducao.md#legendas).

## Requisitos opcionais no `PATH`

- `ffmpeg` e `ffprobe`: duração, codecs, trilhas, miniaturas, legendas, remux e transcode. Sem eles, o servidor continua
  em Direct Play e os campos de mídia ficam vazios.
- `curl`: consultas ao TMDB e download de imagens.
