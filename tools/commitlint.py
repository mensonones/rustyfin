"""Valida mensagens de commit segundo o padrão descrito em CONTRIBUTING.md."""

import argparse
from pathlib import Path
import re
import subprocess
import sys


TYPES = ("feat", "fix", "docs", "refactor", "perf", "test", "build", "ci", "chore", "style", "revert")
HEADER = re.compile(
    rf"({'|'.join(TYPES)})(\([a-z0-9]+(?:-[a-z0-9]+)*\))?!?: \S(?:.*\S)?"
)
AUTOSQUASH = re.compile(r"(fixup|squash|amend)! \S.*")
GIT_REVERT = re.compile(r"This reverts commit (?:[a-f0-9]{40}|[a-f0-9]{64})(?:\.|, reversing)")
HEADER_LIMIT = 72


def lint(message):
    lines = message.splitlines()
    if not lines or not lines[0]:
        return "A mensagem precisa de um título."

    title = lines[0]
    if AUTOSQUASH.fullmatch(title):
        return None
    if (
        title.startswith('Revert "')
        and title.endswith('"')
        and any(GIT_REVERT.fullmatch(line) for line in lines[2:])
    ):
        return None

    if not HEADER.fullmatch(title):
        return f"Use tipo(escopo opcional): descrição. Tipos válidos: {', '.join(TYPES)}."
    if len(title) > HEADER_LIMIT:
        return f"O título deve ter até {HEADER_LIMIT} caracteres (recebido: {len(title)})."
    if len(lines) > 1 and lines[1].strip():
        return "Separe o título do corpo ou rodapé com uma linha em branco."
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file", type=Path, help="arquivo recebido pelo hook commit-msg")
    args = parser.parse_args()

    try:
        result = subprocess.run(
            ["git", "stripspace", "--strip-comments"],
            input=args.file.read_bytes(),
            stdout=subprocess.PIPE,
            check=True,
        )
        error = lint(result.stdout.decode("utf-8"))
    except (OSError, UnicodeError, subprocess.CalledProcessError) as exc:
        print(f"Commit lint: não foi possível ler a mensagem: {exc}", file=sys.stderr)
        return 1

    if error:
        print(f"Commit lint: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
