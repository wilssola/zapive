"""Generate release notes from Git history using only the standard library."""

import argparse
from pathlib import Path
import re
import subprocess


SECTIONS = (
    "Novidades",
    "Melhorias",
    "Correções",
    "Segurança",
    "Alterações importantes",
    "Outras alterações",
)
IGNORED_TYPES = {"chore", "ci", "build", "test", "docs", "style", "bump"}
CATEGORIES = {
    "feat": "Novidades",
    "perf": "Melhorias",
    "refactor": "Melhorias",
    "fix": "Correções",
    "security": "Segurança",
}
CONVENTIONAL = re.compile(
    r"^(?P<type>[a-z]+)(?:\([^()\r\n]+\))?(?P<breaking>!)?:\s+(?P<text>\S.*)$"
)
BREAKING = re.compile(r"^BREAKING(?: CHANGE|-CHANGE):\s*\S", re.MULTILINE)


def generate_notes(commits):
    """Group (subject, body) pairs without rewriting their descriptions."""
    entries = {}
    for subject, body in commits:
        subject = subject.strip()
        if not subject:
            continue
        match = CONVENTIONAL.fullmatch(subject)
        breaking = bool(BREAKING.search(body)) or bool(
            match and match.group("breaking")
        )
        if match:
            kind = match.group("type")
            if kind in IGNORED_TYPES and not breaking:
                continue
            text = match.group("text")
            section = CATEGORIES.get(kind, "Outras alterações")
        else:
            text = subject
            section = "Outras alterações"
        if breaking:
            section = "Alterações importantes"
        key = " ".join(text.split()).casefold()
        if key not in entries:
            entries[key] = (section, text)
        elif breaking:
            entries[key] = (section, entries[key][1])

    blocks = []
    for section in SECTIONS:
        items = [text for category, text in entries.values() if category == section]
        if items:
            blocks.append("## " + section + "\n\n" + "\n".join("- " + text for text in items))
    return "\n\n".join(blocks) + ("\n" if blocks else "")


def git(repo, *args):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        encoding="utf-8",
    ).stdout.strip()


def read_commits(repo, to_tag="HEAD", from_tag=None):
    """Use an exclusive lower bound; without one, discover the previous v* tag."""
    end = git(repo, "rev-parse", "--verify", "--end-of-options", to_tag + "^{commit}")
    if from_tag is None:
        try:
            from_tag = git(repo, "describe", "--tags", "--abbrev=0", "--match", "v*", end + "^")
        except subprocess.CalledProcessError:
            from_tag = None
    revision = end
    if from_tag:
        start = git(repo, "rev-parse", "--verify", "--end-of-options", from_tag + "^{commit}")
        revision = start + ".." + end
    log = git(repo, "log", "--reverse", "--topo-order", "--format=%s%x00%b%x00", revision, "--")
    fields = log.split("\0")
    return [(fields[i].strip(), fields[i + 1]) for i in range(0, len(fields) - 1, 2)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=".")
    parser.add_argument("--from-tag", help="Exclusive lower bound (default: previous v* tag)")
    parser.add_argument("--to-tag", default="HEAD", help="Inclusive upper bound")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    notes = generate_notes(read_commits(args.repo, args.to_tag, args.from_tag))
    Path(args.output).write_text(notes, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
