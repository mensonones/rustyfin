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
O título de pull request segue as mesmas regras de cabeçalho e pode ser usado no squash merge.

## Ativar a validação local

Instale Python 3.8 ou superior e deixe `python` ou `python3` disponível no PATH.
Execute uma vez por clone:

```sh
git config --local core.hooksPath .githooks
```

O hook `commit-msg` valida o título e a separação do corpo.
O GitHub Actions valida o título do pull request e os commits novos em pull requests e pushes para `main`.
O histórico até a versão `v0.3.0` fica fora da validação de adoção.
Merges e reverts gerados pelo Git são aceitos.
Commits de trabalho `fixup!`, `squash!` e `amend!` passam pelo hook local e precisam ser
incorporados com `git rebase --autosquash` antes de enviar o pull request.

Para rodar o validador manualmente:

```sh
python tools/commitlint.py title "feat(hls): adiciona segmentos fMP4"
python tools/commitlint.py range origin/main HEAD --exclude v0.3.0
```

## Versões

Agrupe as mudanças da versão no [changelog](CHANGELOG.md), atualize a versão do
pacote em `Cargo.toml` e `Cargo.lock` e crie uma tag anotada `vX.Y.Z` no commit da versão.
Use PATCH para correções e MINOR para funcionalidades. Na série `0.x`, mudanças
incompatíveis também incrementam a versão menor.
