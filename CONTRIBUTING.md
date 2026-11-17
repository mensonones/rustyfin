# Contribuindo com o Rustyfin

## Mensagens de commit

Use [Conventional Commits](https://www.conventionalcommits.org/pt-br/v1.0.0/).
O título segue `tipo(escopo opcional): descrição` e tem até 72 caracteres.
Use minúsculas no tipo e no escopo. O escopo aceita letras de `a` a `z`, números e hífens.
Escreva a descrição em português, com um verbo no presente, como “adiciona” ou “corrige”.
Mantenha cada commit focado em uma mudança. Separe o corpo ou rodapé do título com uma linha em branco.

| Tipo | Quando usar |
| --- | --- |
| `feat` | Funcionalidade nova |
| `fix` | Correção de comportamento |
| `perf` | Melhoria de desempenho |
| `refactor` | Mudança interna que preserva o comportamento |
| `docs` | Documentação |
| `test` | Testes |
| `build` | Build, empacotamento ou dependências |
| `ci` | Automação de integração contínua |
| `style` | Formatação de código |
| `chore` | Manutenção geral ou preparação de versão |
| `revert` | Reversão escrita manualmente |

```text
feat(hls): adiciona segmentos fMP4
fix(scanner): preserva o catálogo após erro de leitura
docs: atualiza as instruções de conexão
chore(release): prepara a versão 0.3.1
```

Para uma mudança incompatível, use `!` antes dos dois-pontos ou um rodapé `BREAKING CHANGE:`.
Explique o impacto e como migrar no corpo da mensagem.
Use o mesmo formato no título do pull request se ele for usado no squash merge.

## Ativar a validação local

Instale Python 3.8 ou superior e deixe `python` ou `python3` disponível no PATH.
Execute uma vez por clone:

```sh
git config --local core.hooksPath .githooks
```

O hook local `commit-msg` valida o título e a separação do corpo ao criar o commit.
Merges e reverts gerados pelo Git são aceitos.
Commits de trabalho `fixup!`, `squash!` e `amend!` passam pelo hook local e precisam ser
incorporados com `git rebase --autosquash` antes de enviar o pull request.

## Testes

Testes unitários ficam junto do código; os de integração (roteador completo, sem rede) ficam em `tests/`.

O GitHub Actions roda a suíte Rust em pull requests e pushes para `main`.
Ele também roda o Clippy com `-D warnings` (o projeto está sem avisos). Execute os mesmos comandos localmente:

```sh
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## Versões

Agrupe as mudanças da versão no [changelog](CHANGELOG.md), atualize a versão do
pacote em `Cargo.toml` e `Cargo.lock` e crie uma tag anotada `vX.Y.Z` no commit da versão.
Use PATCH para correções e MINOR para funcionalidades. Na série `0.x`, mudanças
incompatíveis também incrementam a versão menor.
