# Assinatura de código e antivírus (Windows)

## Situação atual

**Os executáveis do Rustyfin para Windows ainda não são assinados digitalmente.** Isso tem três efeitos:

| Proteção do Windows | O que acontece com um `.exe` sem assinatura |
|---|---|
| **SmartScreen** | Mostra "O Windows protegeu o computador". Dá para continuar em **Mais informações → Executar assim mesmo** |
| **Smart App Control** | **Bloqueia** o programa e não oferece o "executar assim mesmo". Aparece como "Uma política de Controle de Aplicativo bloqueou este arquivo" (erro 4551) |
| **Microsoft Defender e outros antivírus** | Podem marcar um programa novo e sem reputação como suspeito (falso positivo), principalmente os escritos em Rust, que são menos comuns |

O Smart App Control, quando ativo (pode estar ligado, principalmente em instalações novas do Windows 11), só deixa rodar programas assinados por
uma autoridade que a Microsoft reconhece ou com boa reputação. Foi isso que bloqueou o setup e os binários de teste durante
o desenvolvimento.

## Como conferir que o arquivo é o publicado

Cada pacote da [página de versões](https://github.com/mensonones/rustyfin/releases) tem um `.sha256` ao lado. Compare:

```powershell
Get-FileHash .\Rustyfin-Setup-0.25.0-x64.exe -Algorithm SHA256     # Windows
```

```bash
sha256sum -c rustyfin-v0.25.0-linux-x86_64.tar.gz.sha256            # Linux
shasum -a 256 -c rustyfin-v0.25.0-macos-arm64.tar.gz.sha256         # macOS
```

O valor tem que ser igual ao do arquivo `.sha256`. O `rustyfin.exe` também traz informações de versão (clique com o botão
direito → **Propriedades → Detalhes**: produto "Rustyfin", versão, descrição e a licença MIT). E o código é aberto: dá para
compilar você mesmo (`cargo build --release --locked`).

## Se o Windows bloqueou o programa

1. **SmartScreen:** clique em **Mais informações** e depois em **Executar assim mesmo**.
2. **Smart App Control:** não há botão para liberar um arquivo. As saídas são:
   - **Usar o Docker**, que não depende do `.exe` do Windows (veja os [primeiros passos](primeiros-passos.md));
   - **Esperar a versão assinada** (veja o plano abaixo);
   - desligar o Smart App Control em **Segurança do Windows → Controle de aplicativos e do navegador**. É uma decisão sua
     e **irreversível** (ele só volta reinstalando o Windows), por isso não é a nossa recomendação.

   Observação: um `rustyfin.exe` que você mesmo compila também não tem assinatura, e o Smart App Control pode barrá-lo.

## Se o antivírus marcou o arquivo (falso positivo)

- Confira o SHA-256 (acima). Se bater, o arquivo é o publicado.
- **Microsoft Defender:** envie o arquivo em <https://www.microsoft.com/wdsi/filesubmission>, escolhendo que acredita que ele
  não deveria ser detectado. A Microsoft analisa e atualiza as definições.
- **Outros antivírus:** use o formulário de falso positivo do fabricante. Enviar o arquivo ao
  [VirusTotal](https://www.virustotal.com/) mostra quais mecanismos o detectam.
- Abra uma *issue* informando o antivírus e o nome da detecção, para o projeto acompanhar.

O que o projeto faz para reduzir isso: código aberto e compilado no GitHub Actions a partir da tag (`--locked`), sem
empacotadores de executável, com informações de versão no `.exe`, SHA-256 em cada pacote e o setup instalando só para o seu
usuário, sem pedir administrador.

## Plano: assinatura gratuita pela SignPath Foundation

A [SignPath Foundation](https://signpath.org/) assina gratuitamente projetos de código aberto, com o certificado emitido em
nome dela. Um certificado reconhecido pelo Windows é o que tira o bloqueio do Smart App Control e acaba com o aviso do
SmartScreen (que ainda pode aparecer por um tempo, até o arquivo ganhar reputação).

| Requisito deles | Situação do Rustyfin |
|---|---|
| Licença de código aberto aprovada pela OSI, sem licença comercial paralela | MIT |
| Projeto mantido e já lançado na forma que será assinada | Versões publicadas no GitHub |
| Funcionalidade documentada | Este repositório |
| Sem código proprietário e sem malware | Sim |
| Informações de produto e versão no binário | Feitas (`build.rs`) e conferidas no CI |
| Seção "Code signing policy" no site do projeto | Texto pronto abaixo, para publicar depois da aprovação |
| Autenticação em dois fatores para quem assina | A fazer, na conta do GitHub |
| Pedido de conta gratuita | **A fazer, pelo mantenedor** em <https://signpath.org/apply> |

Cada versão passa por aprovação manual na SignPath, e a aceitação do projeto é decisão deles. Se for aceito, o fluxo de
release envia o executável para assinar e usa o arquivo assinado nos pacotes e no setup. Sem assinatura, o resto continua
funcionando, com os avisos acima.

### Texto da política de assinatura (para o README, depois da aprovação)

> ## Code signing policy
>
> Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).
>
> - **Quem assina:** o mantenedor do repositório (github.com/mensonones), que aprova cada versão manualmente.
> - **O que é assinado:** o `rustyfin.exe` e o `Rustyfin-Setup-X.Y.Z-x64.exe` gerados pelo GitHub Actions a partir de uma
>   tag, sem alterações manuais.
> - **Privacidade:** o Rustyfin não envia informações a outros sistemas, a menos que a pessoa que o instala ou opera peça
>   (por exemplo, ao configurar a chave do TMDB para metadados, ou ao escolher baixar o ffmpeg no setup).
