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


def git(*args, data=None):
    return subprocess.run(
        ["git", *args], input=data, stdout=subprocess.PIPE, check=True
    ).stdout.decode("utf-8")


def lint(message, *, allow_autosquash=False, allow_git_revert=True):
    lines = message.splitlines()
    if not lines or not lines[0]:
        return "A mensagem precisa de um título."

    title = lines[0]
    if allow_autosquash and AUTOSQUASH.fullmatch(title):
        return None
    if (
        allow_git_revert
        and title.startswith('Revert "')
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


def lint_range(base, head, exclude):
    selection = [f"{base}..{head}"]
    if exclude:
        selection.append(f"^{exclude}")
    commits = git("rev-list", "--no-merges", *selection, "--").splitlines()
    failed = False
    for commit in commits:
        message = git("-c", "i18n.logOutputEncoding=UTF-8", "show", "-s", "--format=%B", commit)
        error = lint(message)
        if error:
            print(f"{commit[:12]}: {error}", file=sys.stderr)
            failed = True
    if not failed:
        print(f"Commit lint: {len(commits)} commit(s) validado(s).")
    return int(failed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    message = commands.add_parser("message", help="valida o arquivo recebido pelo hook commit-msg")
    message.add_argument("file", type=Path)
    title = commands.add_parser("title", help="valida um título de pull request")
    title.add_argument("text")
    commits = commands.add_parser("range", help="valida os commits entre BASE e HEAD")
    commits.add_argument("base")
    commits.add_argument("head")
    commits.add_argument("--exclude", help="ignora esta revisão e seus ancestrais")
    args = parser.parse_args()

    try:
        if args.command == "range":
            return lint_range(args.base, args.head, args.exclude)
        if args.command == "message":
            cleaned = git("stripspace", "--strip-comments", data=args.file.read_bytes())
            error = lint(cleaned, allow_autosquash=True)
        else:
            error = lint(args.text, allow_git_revert=False)
    except (OSError, UnicodeError, subprocess.CalledProcessError) as exc:
        print(f"Commit lint: não foi possível ler a mensagem ou o histórico: {exc}", file=sys.stderr)
        return 1

    if error:
        print(f"Commit lint: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
