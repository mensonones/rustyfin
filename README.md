# 🦀 Rustyfin

Servidor de mídia ultraleve em Rust, focado em localhost e **100% compatível com a API de clientes Jellyfin** (como **Moonfin**, **Swiftfin**, **Findroid** e **Jellyfin Media Player**).

---

## ⚡ Diferenciais em Relação ao Jellyfin Tradicional

* **Zero Garbage Collector:** Baixíssimo consumo de memória (15MB a 30MB em repouso).
* **Direct Play First:** Prioriza streaming de chunks via HTTP Range Requests (RFC 7233) com zero cópia de buffer desnecessária.
* **SQLite em Modo WAL:** Leituras e buscas instantâneas sem travar durante varreduras ou atualizações de progresso.
* **Compatibilidade Imediata:** Não requer desenvolvimento de novos aplicativos; funciona diretamente com apps do ecossistema Jellyfin.

---

## 🐳 Executando com Docker

O projeto inclui suporte completo a Docker com build multi-stage e `ffmpeg` embutido para inspeção de codecs:

```bash
# Iniciar o container em segundo plano
docker compose up -d --build

# Ver logs de inicialização e varredura
docker compose logs -f
```

### Configuração de Volumes (`docker-compose.yml`)
* `./data:/data`: Armazena o banco de dados SQLite persistente (`rustyfin.db`).
* `./media:/media`: Pasta onde você pode colocar seus arquivos de vídeo.
* *(Opcional)* Você pode montar sua pasta de vídeos do Windows diretamente no container:
  ```yaml
  volumes:
    - "C:/Users/SEU_USUARIO/Videos:/media/videos:ro"
  ```

---

## 💻 Executando Nativo no Host

### Pré-requisitos
* Rust 1.80+ (`rustup`)
* GCC / MinGW (no Windows)

### Compilação e Execução
```bash
# Compilar e rodar
cargo run

# Ou compilar binário otimizado para produção
cargo build --release
./target/release/rustyfin
```

---

## 👤 Usuários e senhas

* O usuário **`Admin`** é criado na primeira execução **sem senha** (e mantém o progresso de versões anteriores). Contas sem senha **só entram a partir da rede local**; de IP público o login é recusado. Defina uma senha antes de expor o servidor à internet. Defina uma senha para ele em `/admin` → **Usuários**.
* Cada usuário tem **progresso, favoritos e "continuar assistindo" próprios**, e pode ter acesso só a **algumas bibliotecas** (ex.: perfil infantil só com Filmes).
* Senhas guardadas com **Argon2**; cada login gera um **token por dispositivo** (revogado ao trocar a senha, desativar ou sair). 5 senhas erradas bloqueiam novas tentativas por 5 minutos.
* Os apps (Moonfin, Swiftfin, Findroid) pedem usuário e senha; **imagens e segmentos HLS** não exigem token, o resto sim.
* Com um **administrador com senha**, o painel `/admin` (e as rotas `/Library/VirtualFolders*`) passam a exigir login: HTTP Basic com usuário e senha de administrador, ou o token de um administrador logado no app. Sem senha de administrador, vale só a regra de rede local.
* O log de requisições não grava `api_key`/tokens, e séries com o mesmo nome se separam pelo ano da pasta ou do arquivo (`Ghosts (2019)` × `Ghosts (2021)`).
* Dois cuidados: sempre sobra ao menos um administrador ativo, e a lista de usuários aparece na tela de login dos apps (como no Jellyfin).
* **Atualizando de versões antigas:** o app guardava um token fixo que deixou de valer; o primeiro acesso volta à tela "Quem está assistindo?" e basta escolher o usuário.

---

## 📁 Usando pastas do seu PC (Docker)

O container só enxerga o que for **montado como volume**; por isso o seletor de pastas do painel mostra o sistema de arquivos *do container*
(`/media`, `/mnt/...`). Isso é igual ao Jellyfin em Docker; só a instalação nativa mostra as pastas do PC. Para usar uma pasta do Windows:

1. Em `docker-compose.yml`, em `volumes`, adicione (somente leitura): `- "C:/Users/SEU_USUARIO/Videos:/mnt/videos:ro"`
2. `docker compose up -d` e, em `/admin` → Bibliotecas, escolha `/mnt/videos` (ou uma subpasta).

Dica: monte uma pasta "pai" (ex.: `C:/Users/SEU_USUARIO/Downloads:/mnt/downloads:ro`) para poder escolher qualquer subpasta dela sem editar o compose de novo.

---

## 🛠️ Painel de Administração

Abra **`http://localhost:8096/admin`** (ou `http://<ip-do-pc>:8096/admin`):

* **Usuários:** criar, definir senha, desativar, permissões por biblioteca e excluir.
* **Bibliotecas:** criar (Filmes ou Séries), renomear, excluir e adicionar/remover pastas, com seletor de pastas. Mudanças disparam uma varredura na hora.
* **Configurações:** chave e idioma do TMDB, sem recriar o container.
* **Status:** contagem de itens, `ffmpeg`, TMDB, descoberta automática e último scan.

As mesmas operações existem na API do Jellyfin (`POST/DELETE /Library/VirtualFolders`, `.../Paths`, `.../Name`, `POST /Library/Refresh`).
No primeiro uso, as bibliotecas "Filmes" e "Séries" são criadas a partir de `RUSTYFIN_MEDIA_DIRS`; depois, **o banco é a fonte da verdade**
(a variável deixa de valer). No Docker, as pastas são as do container (ex.: `/media`): monte os volumes no `docker-compose.yml`.
Um arquivo pertence à biblioteca da pasta mais específica que o contém; ao excluir uma biblioteca os arquivos não são apagados.

---

## ⚙️ Variáveis de Ambiente

| Variável | Padrão | Descrição |
|---|---|---|
| `RUSTYFIN_PORT` / `RUSTYFIN_HOST` | `8096` / `0.0.0.0` | Endereço de escuta (use `127.0.0.1` para só localhost) |
| `DATABASE_URL` | `sqlite://rustyfin.db?mode=rwc` | Banco SQLite |
| `RUSTYFIN_MEDIA_DIRS` | `/media`, Vídeos do usuário, `./media` | Pastas de mídia, separadas por vírgula |
| `RUSTYFIN_SCAN_CONCURRENCY` | núcleos (2–8) | Arquivos analisados em paralelo (ffprobe) na varredura |
| `RUSTYFIN_SCAN_INTERVAL_SECS` | `300` | Scan automático periódico (`0` desativa; `POST /Library/Refresh` sempre funciona) |
| `RUSTYFIN_WATCH` | `true` | Monitora as pastas por eventos do sistema de arquivos (novos/removidos aparecem em ~3 s). O scan periódico continua como reserva, útil em unidades de rede |
| `RUSTYFIN_DISCOVERY` | `true` | Responde à descoberta automática dos apps (UDP 7359) |
| `RUSTYFIN_ADMIN_PASSWORD` | *(vazio)* | Senha extra do painel `/admin` (HTTP Basic, qualquer usuário), útil antes de criar um administrador com senha. Sem ela e sem admin com senha, o painel só é acessível a clientes de rede local |
| `RUSTYFIN_PUBLIC_URL` | *(IP detectado)* | Endereço anunciado na descoberta. **Em Docker é necessário** (ex.: `http://192.168.1.10:8096` no `.env`), pois o container não conhece o IP do PC |
| `RUSTYFIN_CACHE_DIR` | `./cache` | Miniaturas e imagens baixadas |
| `RUSTYFIN_FFMPEG` | `ffmpeg` | Executável do ffmpeg (remux, transcode e miniaturas) |
| `RUSTYFIN_MAX_TRANSCODES` | `2` | Transcodificações simultâneas (excedentes recebem 503) |
| `RUSTYFIN_VIDEO_ENCODER` | `libx264` | Encoder de vídeo; hardware: `h264_nvenc`, `h264_qsv`, `h264_videotoolbox` |
| `TMDB_API_KEY` | *(vazia)* | Chave v3 ou token v4 do TMDB. Sem ela, nenhum metadado online é buscado |
| `RUSTYFIN_TMDB_LANGUAGE` | `pt-BR` | Idioma dos metadados |

**Reprodução:** o servidor lê o `DeviceProfile` que o cliente envia e escolhe: **Direct Play** (arquivo como está, com Range),
**Remux** (vídeo copiado; só áudio/contêiner mudam, ex.: HEVC+DTS → HEVC+AAC) ou **Transcode** (H.264, quando o codec não é suportado
ou `MaxStreamingBitrate` é menor que o bitrate do arquivo). A saída é HLS (`master.m3u8`) ou stream progressivo (mkv/mp4/ts), conforme
o perfil do cliente. Sem `ffmpeg` o servidor continua em Direct Play. Sessões ociosas são encerradas após 90 s e todo processo morre
quando o cliente desconecta. Legendas não são gravadas no vídeo (*burn-in* não suportado).

Séries aparecem na view **Séries** (Série → Temporada → Episódio), com "Próximo episódio" (`NextUp`) e contagem de não assistidos.

Requisitos opcionais no `PATH`: `ffprobe`/`ffmpeg` (trilhas, duração e miniaturas) e `curl` (TMDB).

Nomes de arquivo reconhecidos: `Filme (2020).mkv`, `The.Matrix.1999.1080p.BluRay.mkv`, `Serie S01E02.mkv`,
`Serie/Season 1/S01E02.mkv`.

---

## 📱 Como Conectar no Moonfin / Swiftfin

1. Abra o app **Moonfin** (iOS / macOS) ou qualquer cliente Jellyfin.
2. Endereço do servidor: `http://localhost:8096` ou `http://<seu-ip-local>:8096`.
3. Usuário: `Admin` (sem senha).
4. Sua biblioteca de vídeos aparecerá pronta para reprodução imediata com avanço e retrocesso instantâneo.

---

### Testando a compatibilidade

* **Simulação de cliente (sem dispositivo):** com o servidor rodando e alguma mídia indexada,
  `python tools/client_probe.py http://127.0.0.1:8096` reproduz a sequência de um cliente Jellyfin
  (descoberta, login, tela inicial, biblioteca, detalhes, reprodução com Range, marcar assistido/favorito)
  e mostra `FALHA` em qualquer rota sem resposta.
* **Dispositivo real:** descubra o IP da máquina (`ipconfig`), permita o `rustyfin.exe` no Firewall do Windows
  (rede privada) e use `http://<ip>:8096` no app. O celular precisa estar na mesma rede Wi‑Fi.

---

## 📖 Documentação & Diretrizes

* **[ROADMAP.md](ROADMAP.md)**: Fases de evolução do projeto (Scanners, Direct Play, Scrobbling, Posters, Transcodificação sob demanda).
* **[RUST_STANDARDS.md](RUST_STANDARDS.md)**: Guia completo de boas práticas e arquitetura para LLMs e desenvolvedores (regras do Tokio, Axum 0.7, SQLx, Zero-Copy streaming e contrato Jellyfin).
* **[.cursorrules](.cursorrules)**: Regras diretas para agentes de IA que editam o projeto.

## 📄 Licença e versões

Licença [MIT](LICENSE). Versões seguem [SemVer](https://semver.org/lang/pt-BR/) e as mudanças ficam no [CHANGELOG](CHANGELOG.md); cada versão é marcada com uma tag `vX.Y.Z`.
