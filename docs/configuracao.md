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
| `RUSTYFIN_MAX_TRANSCODES` | `2` | Transcodificações simultâneas; as excedentes recebem 503. Editável no painel |
| `RUSTYFIN_VIDEO_ENCODER` | `auto` | Encoder de vídeo: `auto` (GPU Intel/AMD via VAAPI se houver, senão `libx264`), `libx264`, `h264_vaapi`, `h264_qsv` (experimental). Veja [aceleração por hardware](reproducao.md#aceleração-por-hardware) |
| `RUSTYFIN_HWACCEL_DEVICE` | `/dev/dri/renderD128` | Dispositivo de renderização da GPU para `h264_vaapi` e `h264_qsv` (Linux) |
| `RUSTYFIN_HWACCEL_DECODE` | `true` | A GPU também decodifica H.264 e HEVC. `false` deixa a decodificação na CPU |

### Acesso

| Variável | Padrão | Descrição |
|---|---|---|
| `RUSTYFIN_ADMIN_PASSWORD` | vazio | Senha extra do painel (HTTP Basic, qualquer usuário). Útil antes de criar um administrador com senha |
| `RUSTYFIN_TOKEN_TTL_DAYS` | `0` (nunca) | Dias sem uso para um dispositivo perder o login e voltar à tela de entrada |

### Metadados (TMDB)

| Variável | Padrão | Descrição |
|---|---|---|
| `TMDB_API_KEY` | vazio | Chave v3 ou token v4 do TMDB. Sem ela, nenhum metadado online é buscado. Também editável no painel |
| `RUSTYFIN_TMDB_LANGUAGE` | `pt-BR` | Idioma dos metadados. A classificação indicativa usa o país do idioma (`pt-BR` → Brasil) e cai para os EUA |
| `RUSTYFIN_TMDB_BASE_URL` | `https://api.themoviedb.org/3` | Endereço da API (útil em testes) |
| `RUSTYFIN_TMDB_IMAGE_BASE` | `https://image.tmdb.org/t/p` | Endereço das imagens |

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
