# Guia para uma LLM: descobrir e recomendar o perfil de execução do Rustyfin

Este guia é para um **assistente de IA** (Claude, Codex ou outro) que ajuda uma pessoa a decidir **como rodar o Rustyfin
na máquina dela**: Docker ou nativo, com GPU ou sem. Siga os passos na ordem. A decisão tem que sair de **uma medição
real**, não de suposição.

> Leia primeiro [escolher perfil](escolher-perfil.md): ele explica os quatro perfis e traz um exemplo medido.

## 0. Regras

1. **Meça antes de recomendar.** Se não puder executar comandos, peça à pessoa que rode o script e cole o JSON. Nunca
   invente números nem afirme que uma GPU funciona sem o teste.
2. **Peça permissão** antes de baixar qualquer arquivo (por exemplo o `ffmpeg` para Windows) e diga o nome, a origem e o
   tamanho. Baixe para uma pasta temporária, nunca para dentro do repositório, e apague depois.
3. **O script não altera a máquina.** Ele grava só na pasta temporária do sistema (`rustyfin-perfil`). Mudar a configuração
   do Rustyfin (`.env`, compose, variáveis) é um passo seguinte, que só se faz com o aval da pessoa.
4. **Diga o que não foi medido.** Perfis "indisponíveis" têm um motivo no JSON: repasse-o, sem escondê-lo.
5. Não envie o JSON a serviços externos. Ele traz o modelo da CPU e da GPU da pessoa.

## 1. Colete o contexto (uma pergunta só, se faltar)

Descubra, olhando a máquina ou perguntando: **sistema operacional** (Windows, Linux, macOS), **se tem Docker**, **qual GPU**
(Intel, AMD, NVIDIA ou nenhuma) e **para que usa o servidor** (só assistir, ou vários aparelhos ao mesmo tempo). O script
detecta quase tudo sozinho; a última pergunta serve para o texto da resposta.

## 2. Prepare e rode

Pré-requisitos: Python 3.8+. Para o Docker, a imagem do Rustyfin precisa existir (`docker compose build`). Para os perfis
nativos é preciso um `ffmpeg`: no Linux e no macOS vale o do `PATH`; **no Windows**, um build `win64-gpl` do projeto
`FFmpeg-Builds` com `h264_qsv` (peça permissão para baixar, ~150 MB).

```bash
# na raiz do repositório
python tools/perfil/escolher_perfil.py --json --out resultado.json
# Windows, com o ffmpeg baixado:
python tools/perfil/escolher_perfil.py --ffmpeg-dir <pasta-do-ffmpeg>\bin --json --out resultado.json
```

Leva cerca de dois minutos. Opções úteis: `--docker-cpus 2` (simula um NAS), `--sem-docker`, `--sem-nativo`, `--image <nome>`,
`--duracao <s>`. Se o Docker for `Docker Desktop`, saiba de antemão que ele **não repassa a GPU Intel** (Windows e macOS).

## 3. Leia o JSON

```text
ambiente.so, .cpu, .nucleos, .memoria_gib, .placas_de_video[]   a máquina
ambiente.docker.disponivel / .desktop / .cpus                   o Docker
perfis.<nome>.disponivel                                        true se foi medido
perfis.<nome>.motivo                                            por que não (quando disponivel = false)
perfis.<nome>.x_tempo_real          velocidade em múltiplos do tempo real (mediana)
perfis.<nome>.nucleos_ocupados      CPU gasta pelo ffmpeg, em núcleos
perfis.<nome>.cpu_pct_do_sistema    o mesmo, em % da máquina
perfis.<nome>.sessoes_simultaneas_estimadas   teto teórico (parte inteira da velocidade)
perfis.<nome>.variacao_pct          melhor contra pior repetição
recomendacao.perfil                 um dos perfis acima, ou null
recomendacao.motivo, .como_aplicar, .alternativa_sem_gpu
notas[]                             avisos (por exemplo, imagem do Rustyfin ausente)
```

`<nome>` é um destes: `docker-cpu`, `docker-gpu`, `docker-nvidia`, `nativo-cpu`, `nativo-gpu`, `nativo-nvidia`. Os de NVIDIA
só aparecem se o script achar uma placa NVIDIA.

## 4. Confira a qualidade da medição

- `variacao_pct` acima de **15%** em algum perfil: a máquina estava ocupada. Peça para fechar o que estiver pesado e rode de
  novo, antes de concluir.
- `recomendacao.perfil` igual a `null`: nada pôde ser medido. Leia cada `motivo` e resolva o primeiro (normalmente falta a
  imagem do Docker ou o `ffmpeg`).
- Se `nativo-cpu` for bem mais rápido que `docker-cpu` (mais de 10%), é provável que o Docker esteja com poucas CPUs
  (confira `ambiente.docker.cpus`) ou com disco lento; diga isso.

## 5. Como a recomendação é decidida

O script aplica estas regras; você deve reproduzi-las para explicar o resultado:

1. A **GPU vence** se for pelo menos **10% mais rápida** que o melhor perfil de CPU, **ou** se tiver pelo menos **85% da
   velocidade** usando **metade ou menos dos núcleos**.
2. Entre perfis de CPU, o **Docker vence** quando fica a **90% ou mais** da velocidade do nativo (é mais simples de instalar e
   de manter).
3. Sem GPU medida, vence o perfil de CPU mais rápido, com a regra 2.

Casos que você deve reconhecer:

| Situação medida | Resposta |
|---|---|
| Windows, GPU Intel, `nativo-gpu` disponível e mais rápido | Rodar **direto no Windows** com Quick Sync. O Docker Desktop não usa a GPU Intel |
| Linux, GPU Intel ou AMD, `docker-gpu` disponível | Docker com `tools/gpu-setup.sh`. Avise que o VAAPI no Linux ainda não foi validado numa GPU real pelo projeto |
| `docker-gpu` indisponível no Linux | Veja o `motivo`: sem `/dev/dri` ou sem permissão. `docker compose exec rustyfin sh /app/gpu-check.sh` diagnostica |
| Só NVIDIA, ou nenhuma GPU | Em geral `docker-cpu`: com NVIDIA o servidor decodifica na CPU e só codifica na GPU, então o ganho tende a ser pequeno. Use a medição de `docker-nvidia` ou `nativo-nvidia` para confirmar; se a NVIDIA vencer, diga que o servidor não tem pipeline completo para ela |
| `docker-cpu` ≈ `nativo-cpu` | Docker. Cite a diferença medida |
| Pouca CPU (NAS) | Rode com `--docker-cpus <n>` e mostre `sessoes_simultaneas_estimadas` |

## 6. Responda

Use este formato curto, preenchido com os **números do JSON**:

```text
Recomendo: <perfil>.

Medi na sua máquina (<cpu>, <gpu>), 1080p H.264 reduzido para 720p:
- <perfil A>: <x>× o tempo real, <n> núcleos de CPU
- <perfil B>: ...
- indisponível: <perfil> (<motivo>)

Por quê: <recomendacao.motivo, em uma frase>.
Como aplicar: <passos de escolher-perfil.md para esse perfil>.
Ressalvas: teste com um vídeo sintético e uma sessão; <o que não foi medido>.
```

Depois ofereça aplicar (com o aval da pessoa) e **confira o resultado**: no log do Rustyfin deve aparecer
`Aceleração por hardware ativa: <encoder>` para perfis de GPU, ou `encoder libx264` para os de CPU.

## 7. O que este teste não cobre

Uma transcodificação por vez e um tipo de vídeo (H.264 de 1080p). Vídeos AV1, legendas de imagem queimadas e HDR deixam
parte do trabalho na CPU mesmo com GPU, e o ganho cai; várias sessões simultâneas dependem também do limite do servidor
(`RUSTYFIN_MAX_TRANSCODES`). Para o desempenho completo do servidor use [`tools/bench/`](../tools/bench/README.md) e, para
validar a GPU de ponta a ponta, [`tools/gpu_test/`](../tools/gpu_test/README.md). Para escolher entre rodar ou não o
Rustyfin no lugar do Jellyfin, veja [migrando do Jellyfin](migrando-do-jellyfin.md).
