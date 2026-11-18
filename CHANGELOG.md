# Changelog

Formato baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/);
versionamento [SemVer](https://semver.org/lang/pt-BR/) (na série 0.x, mudanças incompatíveis sobem o MINOR).

## [Não lançado]

## [0.25.1] - 2026-11-18

### Adicionado
- Setup do Windows: confere se a porta escolhida (padrão 8096) está livre e, se outro servidor de mídia ou o Rustyfin no
  Docker a estiver usando, avisa e sugere a próxima livre (`porta.ps1`). Numa atualização e no modo sem telas, não pergunta.
- Ícone da bandeja: se o servidor cair logo ao iniciar porque a porta está ocupada, o passa-mouse mostra "a porta está
  ocupada por outro programa" e o `rustyfin.log` explica o que fazer, em vez de reiniciar em silêncio.

## [0.25.0] - 2026-11-18

### Adicionado
- **Ícone na bandeja do sistema (Windows):** novo `rustyfin-tray.exe`, com a logo do projeto. Inicia o servidor sem janela de
  console, o mantém no ar (se cair, sobe de novo, com espera crescente) e traz o menu *Abrir o painel*, *Ver o log*, *Abrir a
  pasta de dados*, *Reiniciar o servidor* e *Sair*; o duplo clique abre o painel. Só uma instância por vez. Se a área de
  notificação não existir (sessão sem desktop), o servidor continua no ar, sem o ícone.
- A logo agora é o **ícone** do `rustyfin.exe`, do `rustyfin-tray.exe` e do setup (`rustyfin.ico`, em sete tamanhos),
  embutida pelo `build.rs`, junto com as informações de versão de cada executável.
- `POST /admin/api/desligar`: desligamento gracioso do servidor, **só a partir do próprio computador**. É o que o menu
  *Sair* e *Reiniciar* usam (com senha do painel configurada, eles forçam o encerramento).
- O setup e o `instalar.ps1` instalam o ícone da bandeja (opção marcada por padrão; `-SemBandeja` o dispensa), e a tarefa
  agendada passa a executá-lo. O teste do setup confere que ele sobe e que sai na desinstalação.

### Alterado
- O servidor Windows e os pacotes do Windows incluem o `rustyfin-tray.exe`; o fluxo de release confere as informações de
  versão dos dois executáveis.

## [0.24.2] - 2026-11-18

### Adicionado
- O `rustyfin.exe` do Windows agora traz informações de versão (produto, versão, descrição, direitos e licença), embutidas
  por um `build.rs` próprio, sem depender do SDK do Windows (`link.exe` no MSVC; `windres` no MinGW). Pré-requisito da
  assinatura de código e útil contra falsos positivos de antivírus. O CI confere o resultado.
- [Assinatura de código e antivírus](docs/assinatura-de-codigo.md): por que o Windows pode bloquear o `.exe` (SmartScreen e
  Smart App Control), como conferir o SHA-256, o que fazer com um falso positivo e o plano de assinatura gratuita pela
  SignPath Foundation, com o texto da política de assinatura.

### Alterado
- No fluxo de release, o setup do Windows (Inno Setup) não bloqueia os demais pacotes se falhar.

## [0.24.1] - 2026-11-18

### Corrigido
- Instalador do Windows, achados ao instalar de verdade: o log saía em UTF-16 e com códigos de cor (agora UTF-8 limpo), a
  mensagem final mostrava sempre a porta 8096 (agora lê `RUSTYFIN_PORT`) e o servidor não subia de novo depois de uma queda
  (a tarefa agendada considerava o script bem-sucedido; o script agora reinicia o servidor depois de 5 segundos). O guia
  avisa que, dentro de aplicativos MSIX, `%LOCALAPPDATA%` pode ser virtualizado e que `-Pasta` resolve.

## [0.24.0] - 2026-11-18

### Adicionado
- **Instalação direta no sistema** ([guia](docs/instalacao-nativa.md)): instaladores de serviço em `packaging/` para Windows
  (tarefa agendada, sem administrador), Linux (systemd) e macOS (launchd), com um modelo de configuração `rustyfin.env`.
- Fluxo `release.yml`: a cada tag `vX.Y.Z`, compila o binário para Windows, Linux (x86_64 e aarch64) e macOS (arm64 e x86_64)
  e anexa os pacotes, com `.sha256`, à versão no GitHub.
- `RUSTYFIN_DATA_DIR`: uma pasta só para o banco e o cache, criada se não existir. Sem ela, nada muda.
- Ao subir sem `ffmpeg`, o log diz como instalá-lo no sistema em uso (`winget`, `brew` ou `apt`/`dnf`).

## [0.23.3] - 2026-11-18

### Adicionado
- `tools/perfil/escolher_perfil.py` mede também `docker-nvidia` (`--gpus all`) e `nativo-nvidia` (NVENC) quando acha uma
  placa NVIDIA. Medido numa RTX 3060 no Docker Desktop: o NVENC funciona no container, mas sem ganho (5,2× contra 5,1× no
  processador), porque o servidor decodifica na CPU; um pipeline todo em CUDA ficou mais lento (3,7×).

## [0.23.2] - 2026-11-18

### Alterado
- Imagens com cache no cliente: capas e fundos saem com `Cache-Control: private, max-age=86400` (antes `no-cache`, que
  obrigava o app a perguntar ao servidor a cada exibição). Rolar uma grade ou voltar a uma tela não refaz os pedidos; depois
  de um dia o app revalida com o `ETag` e recebe `304`. As versões redimensionadas já eram guardadas em disco.

## [0.23.1] - 2026-11-18

### Corrigido
- Moonfin na TV Samsung (Tizen): as capas e os fundos apareciam quebrados. A TV os carrega com `<img>`, que não envia
  token, e o servidor respondia 401. Agora `GET /Items/{id}/Images/...` abre sem token **para clientes da rede local**,
  como o Jellyfin faz; de fora da rede e com `RUSTYFIN_IMAGENS_ANONIMAS=false` as imagens voltam a exigir token. O resto
  continua exigindo token.

### Adicionado
- Com `RUST_LOG=rustyfin=debug`, o log registra cada requisição recusada com 401 (só o caminho, sem a query, e o aparelho),
  o que permite achar o motivo de uma tela quebrada num app.

## [0.23.0] - 2026-11-18

### Adicionado
- `tools/perfil/escolher_perfil.py`: mede a transcodificação em Docker e nativo, no processador e na GPU, com os mesmos
  argumentos de `ffmpeg` do servidor, e recomenda o perfil. Saída em tabela ou em JSON (`--json`), sem alterar o sistema.
- [Qual perfil escolher](docs/escolher-perfil.md), com o resultado de uma máquina real (Windows, Intel UHD): Quick Sync
  nativo 8,2× o tempo real usando 0,6 núcleo, contra 5,1× e 11,8 núcleos do Docker no processador.
- [Guia para LLM](docs/guia-llm.md): procedimento para um assistente de IA medir a máquina, interpretar o JSON e responder.

## [0.22.5] - 2026-11-18

### Alterado
- Perfil de release com LTO completa, uma unidade de geração de código e símbolos removidos: o binário caiu de 18,0 MB
  para 10,8 MB e a API sob concorrência ganhou cerca de 7% em vazão (até 16% em algumas rotas). Transcodificação,
  memória em repouso e subida não mudaram. Medição em [docs/desempenho.md](docs/desempenho.md#perfil-de-release-otimizado-v0225).

## [0.22.4] - 2026-11-18

### Corrigido
- Configurações: os cartões Servidor e Metadados online têm a mesma largura e a mesma altura, com a barra de salvar
  alinhada no pé de cada um, e não deixam mais uma coluna vazia em telas largas.

## [0.22.3] - 2026-11-18

### Corrigido
- Diagnóstico: o fim da tela deixava uma coluna vazia e cartões de larguras diferentes. Agora são duas colunas iguais, com
  as seções recolhidas empilhadas à esquerda e o diagnóstico antes do Play à direita; em telas estreitas vira uma coluna.

## [0.22.2] - 2026-11-18

### Adicionado
- Aba **Acervo**: miniatura da capa de cada título na lista, carregada sob demanda conforme a rolagem. Títulos sem capa
  local nem do TMDB mostram um quadro do próprio vídeo.
- Rota `GET /admin/api/acervo/{id}/capa` para o painel (as outras rotas de imagem exigem o token de um usuário).

## [0.22.1] - 2026-11-18

### Alterado
- Aba **Acervo**: a barra de paginação aparece sempre, com o intervalo mostrado ("Mostrando 1 a 50 de 120"), botões
  Primeira, Anterior, Próxima e Última e a escolha de 25, 50, 100 ou 200 títulos por página. O tamanho da página é
  pedido à API (`porPagina`, de 10 a 200); busca, filtros e páginas continuam sendo resolvidos no servidor.

## [0.22.0] - 2026-11-18

### Adicionado
- Aba **Acervo** no painel: lista paginada de filmes e episódios com busca por título, série ou caminho, filtro por
  biblioteca e por situação (sem metadados, sem capa, sem legenda, assinatura pendente, sem cenas, não analisados), com
  as contagens de cada filtro. Cada linha mostra a biblioteca, o formato, os codecs, o tamanho e a duração, e tem
  ações para indexar cenas e áudio, ajustar a legenda e copiar o caminho. Também está na paleta de comandos.
- Rota `GET /admin/api/acervo`, com teste de integração (selos, busca, filtros, biblioteca e paginação).

### Documentação
- Novo guia [Primeiros passos](docs/primeiros-passos.md): instalação com Docker, como montar as pastas de mídia, painel,
  conexão do app, GPU, atualização, backup e acesso de fora de casa.
- Novo [Migrando do Jellyfin](docs/migrando-do-jellyfin.md): quando vale a pena trocar (e quando não), o que o Rustyfin tem a
  mais e a menos e como testar lado a lado sem risco.
- README com a seção "Por que usar" e um início rápido que cobre a montagem das pastas.

## [0.21.1] - 2026-11-18

### Alterado
- O painel aproveita a largura da tela: o conteúdo vai até 1760 px, com margens que crescem com a janela. Bibliotecas,
  usuários e sessões viram grades de cartões, e Configurações e Diagnóstico passam a usar duas colunas em telas largas.

## [0.21.0] - 2026-11-18

### Alterado
- Painel de administração redesenhado: tema Coral com modo claro e escuro (segue o sistema e lembra a escolha), barra
  superior com as cinco abas, paleta de comandos (Ctrl K) e avisos só do que precisa de atenção (FFmpeg ausente, painel
  sem senha, assinaturas pendentes).
- Listas de bibliotecas, usuários e sessões com estados de carregamento, vazio e erro; ações raras em menus; barra de
  varredura com progresso; configurações com indicação de alterações não salvas e botão de descartar.
- `confirm()` e `prompt()` do navegador trocados por diálogos próprios, acessíveis por teclado.
- A página `/cenas` usa o mesmo visual e o mesmo tema do painel.
- Todos os ids e chamadas à API do painel foram preservados; só o seletor de seção do celular deu lugar às abas, que
  rolam de lado.

### Adicionado
- Fontes Geist e Geist Mono (licença OFL) e ícones Lucide (ISC) embutidos no binário, sem CDN. Rotas públicas
  `/assets/fontes/geist-latin.woff2` e `/assets/fontes/geist-mono-latin.woff2`, com cache longo.
- Teste de integração das rotas de fontes.

## [0.20.4] - 2026-11-18

### Adicionado
- O painel avisa, ao buscar um título para indexar áudio e cenas, quantos arquivos ainda aguardam a assinatura (hash) em
  segundo plano, em vez de só falhar na hora de indexar.

### Testado
- Indexação e busca de cenas de ponta a ponta: lê o SRT, conta os trechos, respeita o progresso e os spoilers, substitui o
  índice ao reindexar e não devolve o texto antigo quando a legenda muda.
- Recusas de indexar cenas e áudio (título inexistente ou sem assinatura), versões preparadas (rotas vazias, chave
  desconhecida, só observa títulos com hash, uma sessão por usuário, ocupado enquanto há reprodução, expiração) e a
  assinatura de áudio (sem hash, assinatura já existente, arquivo alterado, trechos sem informação).
- Medida da busca de cenas: 0,2 a 0,3 s com 1 milhão de falas e 0,6 a 1,0 s com 3 milhões.

## [0.20.3] - 2026-11-18

### Corrigido
- Ajustar uma legenda SRT logo depois da primeira varredura salvava um ajuste que era **descartado em silêncio** quando o hash do
  vídeo passava a existir (a assinatura do ajuste inclui esse hash, que desde a v0.17.3 é calculado em segundo plano). Agora a
  prévia e o salvamento respondem 409 até o hash existir.
- As mensagens do painel para indexar áudio e cenas e para ajustar legendas explicam que a assinatura do arquivo ainda está
  sendo calculada, em vez de um erro genérico.

### Adicionado
- `GET /admin/api/status` informa `scan.signaturesPending`, quantos arquivos ainda esperam o hash.
- `RUSTYFIN_PREPARO_MAX_BYTES` documentado na tabela de variáveis (`0` impede novos preparos).

### Adicionado
- `tools/gpu_test/gpu_test.py`: testes do transcode por GPU para rodar fora do Docker (Quick Sync no Windows, VAAPI no Linux). Gera
  vídeos de teste, inclusive uma legenda PGS, e confere formatos, HDR, ABR, avanço, fMP4, 4K, AV1, entrelaçado, legenda de
  imagem queimada, sessões simultâneas e a volta para software.

### Testado
- Na GPU Intel real (Quick Sync, Windows nativo) com arquivos gerados: 4K (H.264 e HEVC), AV1, H.264 entrelaçado e legenda de
  imagem (PGS) queimada, em H.264 e em HEVC. Todos funcionam; resultados em `docs/reproducao.md`.

## [0.20.2] - 2026-11-18

### Corrigido
- H.264 de **10 bits** (Hi10P) falhava com a GPU Intel: ela não decodifica esse perfil e o ffmpeg não cai para a CPU sozinho.
  Agora só o HEVC usa 10 bits na decodificação por hardware; o H.264 de 10 bits é decodificado na CPU e codificado na GPU.

### Adicionado
- **Volta automática para software.** Quando o ffmpeg com GPU encerra com erro numa sessão HLS, o servidor refaz a sessão com
  `libx264` no mesmo ponto e registra um aviso, em vez de devolver 404 ou repetir o mesmo erro.

### Testado
- Numa GPU Intel real (Quick Sync, Windows nativo): H.264 8 e 10 bits, HEVC 8 e 10 bits, HDR10 com tone mapping, VP9, segmentos
  de 6 s exatos, avanço, ABR, fMP4, stream progressivo e três sessões simultâneas (4,8× cada com ~34% da CPU, contra 2,5× e ~97%
  do `libx264`).

## [0.20.1] - 2026-11-18

### Adicionado
- `tools/gpu-check.sh` (também em `/app/gpu-check.sh` na imagem): diagnóstico da GPU dentro do container. Testa o
  dispositivo, a permissão, o `vainfo`, a codificação e a decodificação por hardware e a velocidade contra o `libx264`, e
  imprime um resumo para colar numa issue.
- No Docker Desktop (WSL2) o log avisa que a GPU Intel não é repassada ao container e aponta o caminho nativo do Windows.
- Guia do Rustyfin nativo no Windows com o Quick Sync (`docs/reproducao.md#windows`).

### Alterado
- O aviso "ffmpeg falhou ao gerar miniatura" agora inclui a mensagem de erro do ffmpeg.

## [0.20.0] - 2026-11-18

### Adicionado
- **Limite automático de transcodificações.** Sem `RUSTYFIN_MAX_TRANSCODES` e sem valor salvo no painel, o servidor calcula o
  limite pelo hardware: 4 com GPU; senão metade das CPUs, de 2 a 8 (respeita o `--cpus` do Docker). O log e o painel mostram
  o valor em uso. Antes o padrão era sempre 2.
- **GPU no Docker em dois comandos.** `tools/gpu-setup.sh` detecta o dispositivo e o grupo `render` do host e grava o `.env`
  para o novo `docker-compose.gpu.yml`, que repassa `/dev/dri` e o `group_add`. Depois, `docker compose up -d`.
- Quando a GPU existe mas o container não tem permissão para abri-la, o log diz qual `group_add` falta, em vez de uma falha
  genérica do ffmpeg.

### Corrigido
- O limite de transcodificações salvo no painel era ignorado ao reiniciar o servidor (a subida só lia o ambiente). Agora o
  valor salvo vale, depois o do ambiente, depois o automático.

## [0.19.0] - 2026-11-18

### Adicionado
- **Decodificação por hardware.** Com um encoder de GPU ativo, o Rustyfin agora também decodifica H.264 e HEVC (8 e 10 bits,
  4:2:0) na GPU e mantém os quadros lá até o encoder (`scale_vaapi`/`vpp_qsv`), como o Jellyfin. Na subida o servidor testa a
  decodificação com um vídeo de exemplo; se falhar, a GPU só codifica. Legendas queimadas, HDR para SDR e ABR seguem pela
  CPU (decodifica na CPU e só o encode vai para a GPU). `RUSTYFIN_HWACCEL_DECODE=false` desliga a decodificação na GPU.
- **Quick Sync no Windows.** Com o Rustyfin rodando direto no Windows (fora do Docker) e um ffmpeg com QSV, o encoder `auto`
  escolhe `h264_qsv` na GPU Intel (via D3D11), sem `/dev/dri`.
- Controle de taxa por fabricante, como o Jellyfin: `-rc_mode VBR` no VAAPI (`CBR` com o driver `i965`, detectado na
  subida) e `maxrate = bitrate + 1` no Quick Sync.

### Corrigido
- Com o Quick Sync, os segmentos HLS saíam com ~10 s em vez de 6 s (o encoder ignora `-force_key_frames`), então os cortes não
  batiam com a playlist e a reprodução ficava lenta. Agora o intervalo de quadros-chave vem do fps do vídeo (`-g` e
  `-keyint_min`), como no Jellyfin.
- O `hwupload` do Quick Sync não usa mais `extra_hw_frames=64`, que o D3D11 do Windows recusa.

### Medido
- Numa Intel UHD (Tiger Lake) com o Rustyfin nativo no Windows, transcodificando 3 minutos de um filme 1080p para 700 kbps:
  `libx264` ≈ 7× o tempo real usando ≈ 85% da CPU do sistema; Quick Sync com decodificação na GPU ≈ 11,5× usando ≈ 21%.
  O modo "só codifica na GPU" fez 6,7× com ≈ 52%. O VAAPI no Linux ainda não foi exercitado numa GPU real.

## [0.18.0] - 2026-11-18

### Adicionado
- Aceleração por hardware com GPU Intel (e AMD) no Linux. O encoder padrão passa a ser `auto`: se o container enxerga a
  GPU (`/dev/dri`) e um quadro de teste codifica, o transcode usa `h264_vaapi`; senão segue em software (`libx264`).
  Também aceita `h264_vaapi` e `h264_qsv` (Quick Sync, experimental) escolhidos à mão; se o escolhido não funcionar, o
  servidor avisa no log e usa `libx264` em vez de quebrar a reprodução. O painel mostra o encoder realmente em uso.
- Os filtros (legendas queimadas, HDR para SDR, ABR) rodam na CPU e só o resultado sobe para a GPU, então todos os modos de
  transcode funcionam com o encoder de hardware. O vídeo copiado (remux) nunca abre a GPU.
- `RUSTYFIN_HWACCEL_DEVICE` (padrão `/dev/dri/renderD128`) escolhe o dispositivo de renderização.
- A imagem Docker (amd64) inclui os drivers Intel (`intel-media-va-driver-non-free`, `i965-va-driver`), as bibliotecas do
  Quick Sync e o `vainfo` (+69 MB). O `docker-compose.yml` traz o exemplo comentado para repassar a GPU.

### Observação
- Esta versão foi validada por testes dos argumentos do ffmpeg e dos fallbacks, mas **não numa GPU Intel real**: o Docker
  Desktop no Windows não repassa `/dev/dri` ao container. Quem testar numa máquina Linux com GPU Intel, por favor relate o
  resultado.

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
