# Instalação direta no sistema (sem Docker)

O Rustyfin é um único binário. Dá para rodá-lo **direto no Windows, no Linux ou no macOS**, sem Docker. Esta página
mostra como, nos três sistemas, e como deixá-lo subindo sozinho como serviço.

## Docker ou direto?

| | Docker | Direto no sistema |
|---|---|---|
| Instalação | Um comando; o `ffmpeg` e os drivers vêm na imagem | Você instala o `ffmpeg` |
| Atualizar | `git pull` e `docker compose up -d --build` | Baixar o binário novo |
| **GPU Intel no Windows** | **Não funciona** (o Docker Desktop não a repassa) | **Quick Sync funciona** |
| GPU Intel/AMD no Linux | Funciona com `tools/gpu-setup.sh` | Funciona (VAAPI) |
| Isolamento | Container | Roda na sua conta ou como um usuário de serviço |

Em resumo: **no Windows com GPU Intel, rodar direto transcodifica 1,6× mais rápido e usa cerca de 20× menos CPU**; nas
demais situações, o Docker é o mais simples. Para medir na sua máquina, use o
[script de perfis](escolher-perfil.md). Se o Docker serve, comece pelos [primeiros passos](primeiros-passos.md).

## O que você precisa

| Item | Para quê | Obrigatório? |
|---|---|---|
| O binário do Rustyfin | O servidor | Sim |
| `ffmpeg` e `ffprobe` | Transcodificar, ler codecs, miniaturas, legendas embutidas | Não, mas sem eles só há Direct Play |
| `curl` | Metadados do TMDB | Só com o TMDB. Já vem no Windows 10+ e no macOS |

**Instalar o ffmpeg**

| Sistema | Comando |
|---|---|
| Windows | `winget install Gyan.FFmpeg` (abra um terminal novo depois, para o `PATH` valer) |
| macOS | `brew install ffmpeg` |
| Debian, Ubuntu, Raspberry Pi OS | `sudo apt install ffmpeg` |
| Fedora | `sudo dnf install ffmpeg` (ative o RPM Fusion se o pacote não existir) |
| Arch | `sudo pacman -S ffmpeg` |

Se o `ffmpeg` não estiver no `PATH`, aponte-o com `RUSTYFIN_FFMPEG` (o `ffprobe` precisa estar na mesma pasta ou no `PATH`).
Ao subir sem ele, o log diz como instalar.

## 1. Obter o binário

**Pronto para baixar:** na [página de versões](https://github.com/mensonones/rustyfin/releases) há um pacote por sistema
(`windows-x86_64`, `linux-x86_64`, `linux-aarch64`, `macos-arm64`, `macos-x86_64`), com o binário e os instaladores de
serviço. Confira o `.sha256` ao lado. *(Os pacotes são montados automaticamente a cada tag; para as versões anteriores à
v0.24.0, compile.)*

**Ou compile** (Rust 1.85 ou mais novo):

```bash
git clone https://github.com/mensonones/rustyfin.git
cd rustyfin
cargo build --release          # o binário fica em target/release/rustyfin (rustyfin.exe no Windows)
```

## 2. Rodar na hora

```bash
./rustyfin                     # Linux, macOS (target/release/rustyfin se você compilou)
.\rustyfin.exe                 # Windows
```

Abra **http://localhost:8096/admin**. O resto do uso é igual ao do Docker: [primeiros passos](primeiros-passos.md) a partir
do passo 4. As pastas de mídia são as do próprio computador (sem montar volume): escolha-as direto no seletor do painel.

Por padrão o banco (`rustyfin.db`) e o cache ficam na **pasta de onde você iniciou** o programa. Para guardá-los num lugar
fixo, defina `RUSTYFIN_DATA_DIR`:

```bash
RUSTYFIN_DATA_DIR=/var/lib/rustyfin ./rustyfin              # Linux, macOS
$env:RUSTYFIN_DATA_DIR = "$env:LOCALAPPDATA\Rustyfin\dados"; .\rustyfin.exe     # Windows (PowerShell)
```

A pasta é criada se não existir; `DATABASE_URL` e `RUSTYFIN_CACHE_DIR`, se definidas, continuam valendo. Todas as variáveis
estão em [configuração](configuracao.md#variáveis-de-ambiente).

## 3. Subir sozinho, como serviço

Os instaladores vêm no pacote (e em [`packaging/`](../packaging/)). Todos leem a configuração de um arquivo
`rustyfin.env`, no mesmo formato `CHAVE=VALOR` do `.env` do Docker, e guardam os dados numa pasta própria. O modelo está em
[`packaging/rustyfin.env.example`](../packaging/rustyfin.env.example).

### Windows

Sem administrador: o servidor sobe **quando você entra na conta** (tarefa agendada) e reinicia sozinho se cair.

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\instalar.ps1 -Binario .\rustyfin.exe
```

- Tudo fica em `%LOCALAPPDATA%\Rustyfin`: o `rustyfin.exe`, o `rustyfin.env` (a configuração), os `dados` e o `rustyfin.log`.
- Se o celular ou a TV não encontrarem o servidor, libere as portas na rede privada (precisa de um PowerShell **como
  administrador**): `.\packaging\windows\instalar.ps1 -Binario .\rustyfin.exe -Firewall`.
- `-SomenteMostrar` lista o que o instalador faria, sem alterar nada. `-Remover` tira a tarefa e o binário e mantém os
  dados; `-Remover -ApagarDados` apaga tudo.
- **GPU Intel (Quick Sync):** instale um `ffmpeg` com `h264_qsv` (o `winget install Gyan.FFmpeg` serve). O encoder `auto`
  escolhe o Quick Sync e o log mostra `Aceleração por hardware ativa: h264_qsv`. Veja [reprodução → Windows](reproducao.md#windows).

Para atualizar, rode o instalador de novo com o binário novo.

### Linux (systemd)

```bash
sudo sh packaging/linux/instalar.sh ./rustyfin
```

- Instala em `/usr/local/bin/rustyfin`, cria o usuário `rustyfin` (sem login), guarda os dados em `/var/lib/rustyfin` e a
  configuração em `/etc/rustyfin/rustyfin.env`, e ativa o serviço. Logs: `journalctl -u rustyfin -f`.
- **O usuário `rustyfin` precisa ler as suas pastas de mídia** (por exemplo, `chmod -R o+rX` na pasta, ou um grupo comum).
- **GPU Intel ou AMD (VAAPI):** o instalador acrescenta o usuário aos grupos `render` e `video`, e o encoder `auto` usa a
  GPU se ela existir. Teste com `vainfo` (`sudo apt install vainfo`); sem driver, o servidor transcodifica na CPU, sem erro.
- Atualizar: rode o instalador de novo com o binário novo. Remover: `sudo sh packaging/linux/instalar.sh --desinstalar`
  (mantém os dados; some com `--apagar-dados`).
- Sem systemd (Alpine, containers, NAS): rode o binário direto, com `RUSTYFIN_DATA_DIR`, no gerenciador do seu sistema.

### macOS (launchd)

```bash
sh packaging/macos/instalar.sh ./rustyfin
```

- Instala um agente que sobe **quando você entra na conta** e reinicia se cair. Tudo fica em
  `~/Library/Application Support/Rustyfin` (binário, `rustyfin.env`, `dados` e `rustyfin.log`).
- Se o macOS bloquear o binário baixado ("desenvolvedor não identificado"), o instalador já remove a marca de quarentena;
  se ainda assim bloquear, abra **Ajustes do Sistema → Privacidade e Segurança** e permita.
- GPU: o macOS não tem VAAPI nem Quick Sync. O `auto` usa o processador; o encoder `h264_videotoolbox` existe no `ffmpeg`
  do Homebrew, mas o Rustyfin **não o testou**.

### Outros sistemas (Raspberry Pi, FreeBSD, NAS)

O pacote `linux-aarch64` serve para Raspberry Pi e outros ARM de 64 bits. Para FreeBSD e demais, compile com
`cargo build --release` e rode o binário com `RUSTYFIN_DATA_DIR`. A transcodificação por GPU depende do `ffmpeg` e do
driver do sistema.

## Rede e portas

| Porta | Uso |
|---|---|
| `8096/tcp` | O app e o painel |
| `7359/udp` | Descoberta automática dos apps |

Fora do Docker o servidor **detecta sozinho o IP da rede**; não precisa do `RUSTYFIN_PUBLIC_URL` (só se ele escolher a placa
de rede errada). No Windows, o Firewall pergunta na primeira vez: permita na **rede privada**.

## Problemas comuns

| Sintoma | Solução |
|---|---|
| "Address already in use" ao subir | Já há algo na porta 8096 (outro Rustyfin, o Jellyfin). Mude com `RUSTYFIN_PORT` |
| Só há Direct Play, sem transcodificação | O `ffmpeg` não foi achado; veja a linha `ffmpeg não encontrado` no log, que diz como instalar |
| A biblioteca está vazia (Linux) | O usuário do serviço não lê a pasta de mídia: ajuste as permissões |
| O serviço não sobe | Windows: `Get-Content $env:LOCALAPPDATA\Rustyfin\rustyfin.log`; Linux: `journalctl -u rustyfin`; macOS: o `rustyfin.log` |
| Mais ajuda | [Solução de problemas](solucao-de-problemas.md) |

> **Estado da validação:** o servidor foi exercitado direto no **Windows** (inclusive o Quick Sync, com `RUSTYFIN_DATA_DIR`) e
> no Linux dentro do Docker. O instalador do Windows foi conferido na simulação (`-SomenteMostrar`) e na sintaxe, e os de
> Linux e de macOS só na sintaxe; os pacotes por sistema são montados pelo GitHub Actions e ainda não tiveram uma rodada
> real. Se algo falhar, abra uma *issue* com o sistema, a versão e o trecho do log.
