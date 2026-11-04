import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from generate_changelog import (
    DOWNLOADS_HEADING,
    Commit,
    checksum_files,
    checksums_text,
    downloads_section,
    generate_notes,
    git,
    read_commits,
)

REPO = "owner/repo"
SHA = "0123456789abcdef0123456789abcdef01234567"


class GenerateNotesTests(unittest.TestCase):
    def test_categories_scope_labels_and_prefix_removal(self):
        notes = generate_notes([
            ("feat(chat): Adicionar busca", ""),
            ("perf: Reduzir latência", ""),
            ("refactor(ui): Simplificar navegação", ""),
            ("fix(sync): Corrigir sincronização (#12)", ""),
            ("security: Validar origem", ""),
            ("feat!: Remover API antiga", ""),
            ("Atualizar janela principal", ""),
            ("custom(ui): Ajustar janela", ""),
        ])
        self.assertEqual(notes, (
            "## Alterações importantes\n\n- Remover API antiga\n\n"
            "## Novidades\n\n- **chat**: Adicionar busca\n\n"
            "## Melhorias\n\n- Reduzir latência\n- **ui**: Simplificar navegação\n\n"
            "## Correções\n\n- **sync**: Corrigir sincronização (#12)\n\n"
            "## Segurança\n\n- Validar origem\n\n"
            "## Outras alterações\n\n- Atualizar janela principal\n- **ui**: Ajustar janela\n"
        ))

    def test_maintenance_merges_and_version_bumps_are_ignored(self):
        commits = [(kind + "(release): v1.2.3", "") for kind in
                   ("chore", "ci", "build", "test", "docs", "style", "bump")]
        commits.append(("Merge pull request #3 from owner/topic", "feat: x"))
        self.assertEqual(generate_notes(commits), "")
        self.assertEqual(generate_notes([]), "")

    def test_breaking_maintenance_and_body_trailers(self):
        notes = generate_notes([
            ("chore!: Remover compatibilidade", ""),
            ("docs(api): Alterar contrato", "Detalhes\n\nBREAKING CHANGE: contrato removido"),
            ("fix: Alterar formato", "BREAKING-CHANGE: formato incompatível"),
            ("Mensagem livre", "BREAKING CHANGE: migração necessária"),
        ])
        self.assertEqual(notes.count("## "), 1)
        self.assertTrue(notes.startswith("## Alterações importantes\n"))
        self.assertIn("- Mensagem livre\n", notes)
        self.assertEqual(notes.count("\n- "), 4)
        self.assertIn("  - **Migração:** contrato removido", notes)
        self.assertIn("  - **Migração:** migração necessária", notes)

    def test_migration_text_spans_the_whole_paragraph(self):
        notes = generate_notes([(
            "feat(storage)!: Alterar o formato",
            "Motivo.\n\nBREAKING CHANGE: o histórico antigo\nprecisa ser exportado.\n\nOutro parágrafo",
        )])
        self.assertIn("  - **Migração:** o histórico antigo precisa ser exportado.\n", notes)
        self.assertNotIn("Outro parágrafo", notes)

    def test_deduplication_and_breaking_priority(self):
        notes = generate_notes([
            ("feat(ui): Melhorar busca", ""),
            ("fix: melhorar   busca", ""),
            ("chore!: MELHORAR BUSCA", ""),
            ("feat: Melhorar busca", ""),
        ])
        self.assertEqual(notes, "## Alterações importantes\n\n- MELHORAR BUSCA\n")

    def test_nonconventional_subjects_are_preserved_without_body(self):
        subjects = ["Melhorar interface: busca", "fix:sem espaço", "Bump version to 2", "Merge branch 'topic'"]
        notes = generate_notes([(subject, "Texto que não deve aparecer") for subject in subjects])
        self.assertEqual(notes, "## Outras alterações\n\n" + "\n".join(
            "- " + s for s in subjects if not s.startswith("Merge")) + "\n")

    def test_first_body_paragraph_only_for_feat_fix_perf_security(self):
        body = "Primeiro parágrafo\nem duas linhas.\n\nSegundo parágrafo.\n\nCo-authored-by: A <a@x.y>"
        notes = generate_notes([
            ("feat(a): Um", body), ("fix(b): Dois", body), ("perf(c): Três", body),
            ("security(d): Quatro", body), ("refactor(e): Cinco", body), ("Seis", body),
        ])
        self.assertEqual(notes.count("  - Primeiro parágrafo em duas linhas."), 4)
        self.assertNotIn("Segundo", notes)
        self.assertNotIn("Co-authored-by", notes)

    def test_long_body_is_truncated_on_a_word(self):
        notes = generate_notes([("feat: Longo", "palavra " * 200)])
        detail = [line for line in notes.splitlines() if line.startswith("  - ")][0]
        self.assertTrue(detail.endswith("…"))
        self.assertLess(len(detail), 420)

    def test_links_use_repository_commit_and_pull_request(self):
        commit = Commit("fix(sync): Corrigir (#12)", "", SHA, "Ana", "ana@example.invalid", None)
        self.assertIn("- **sync**: Corrigir (#12, `0123456`)", generate_notes([commit]))
        linked = generate_notes([commit], REPO)
        self.assertIn(
            "([#12](https://github.com/owner/repo/pull/12), "
            "[`0123456`](https://github.com/owner/repo/commit/" + SHA + "))",
            linked,
        )
        self.assertNotIn("Corrigir (#12)", linked)

    def test_mentions_are_neutralised(self):
        notes = generate_notes([("fix: Corrigir @alguem e a@b.c e `@code`", "")])
        self.assertNotIn(" @alguem", notes)
        self.assertIn("a@b.c", notes)
        self.assertIn("`@code`", notes)

    def test_contributors_and_compare_link(self):
        commits = [
            Commit("feat: A", "", SHA, "Ana", "1+ana@users.noreply.github.com", None),
            Commit("feat: B", "", SHA, "Bruno *B*", "bruno@example.invalid", None),
            Commit("feat: C", "", SHA, "Ana", "ana@example.invalid", None),
            Commit("fix: D", "", SHA, "github-actions[bot]", "bot@example.invalid", None),
        ]
        notes = generate_notes(commits, REPO, ("v1.0.0", "v1.1.0"))
        self.assertIn("Obrigado a [@ana](https://github.com/ana), Bruno B.", notes)
        self.assertTrue(notes.endswith(
            "**Changelog completo**: "
            "[v1.0.0...v1.1.0](https://github.com/owner/repo/compare/v1.0.0...v1.1.0)\n"))
        first = generate_notes(commits, REPO, (None, "v1.0.0"))
        self.assertIn("/tree/v1.0.0", first)
        self.assertEqual(generate_notes([("chore: x", "")], REPO, ("v1", "v2")), "")


class DownloadsTests(unittest.TestCase):
    def test_checksums_table_and_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            for name, data in (("Zapive-Setup-1.2.3.exe", b"a"), ("Zapive-1.2.3-x86_64.AppImage", b"bb"),
                               ("Zapive-1.2.3.dmg", b"c" * 2048), ("zapive-windows-x86_64.zip", b"d"),
                               ("notes.md", b"ignored")):
                (Path(directory) / name).write_bytes(data)
            files = checksum_files(directory)
            self.assertEqual([f[0] for f in files], [
                "Zapive-1.2.3-x86_64.AppImage", "Zapive-1.2.3.dmg",
                "Zapive-Setup-1.2.3.exe", "zapive-windows-x86_64.zip"])
            sums = checksums_text(files)
            self.assertIn(hashlib.sha256(b"a").hexdigest() + "  Zapive-Setup-1.2.3.exe\n", sums)
            section = downloads_section(files, REPO, "v1.2.3")
        self.assertTrue(section.startswith("## " + DOWNLOADS_HEADING + "\n"))
        self.assertLess(section.index("Windows (x86_64)"), section.index("Linux (x86_64)"))
        self.assertLess(section.index("Linux (x86_64)"), section.index("macOS (Apple Silicon)"))
        self.assertIn("https://github.com/owner/repo/releases/download/v1.2.3/Zapive-Setup-1.2.3.exe", section)
        self.assertIn("2.0 KiB", section)
        self.assertIn("sha256sum --ignore-missing -c SHA256SUMS.txt", section)
        self.assertIn("Get-FileHash", section)

    def test_heading_stays_hidden_from_the_in_app_changelog(self):
        # src/update.rs drops any section whose heading contains "sha-256".
        self.assertIn("sha-256", DOWNLOADS_HEADING.lower())


class GitHistoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        git(self.repo, "init")
        git(self.repo, "config", "user.name", "Changelog test")
        git(self.repo, "config", "user.email", "test@example.invalid")
        git(self.repo, "config", "commit.gpgsign", "false")
        git(self.repo, "config", "tag.gpgsign", "false")
        git(self.repo, "config", "core.hooksPath", str(self.repo / "no-hooks"))

    def commit(self, subject, body=""):
        args = ["commit", "--allow-empty", "-m", subject]
        if body:
            args += ["-m", body]
        git(self.repo, *args)

    def pairs(self, *args):
        return [(c.subject, c.body.strip()) for c in read_commits(self.repo, *args)]

    def test_first_release_with_single_root_commit(self):
        self.commit("feat: Primeira função")
        git(self.repo, "tag", "v1.0.0")
        commits = read_commits(self.repo, "v1.0.0")
        self.assertEqual([c.subject for c in commits], ["feat: Primeira função"])
        self.assertEqual(len(commits[0].sha), 40)
        self.assertEqual(commits[0].author, "Changelog test")

    def test_tag_interval_discovery_and_cli(self):
        self.commit("feat: Antiga")
        git(self.repo, "tag", "v1.0.0")
        self.commit("fix: Corrigir ação", "BREAKING CHANGE: formato novo")
        git(self.repo, "tag", "unrelated")
        self.commit("chore(release): v2.0.0")
        git(self.repo, "tag", "-a", "v2.0.0", "-m", "Release")
        self.commit("feat: Não publicada")
        explicit = read_commits(self.repo, "v2.0.0", "v1.0.0")
        self.assertEqual(read_commits(self.repo, "v2.0.0"), explicit)
        self.assertEqual(len(explicit), 2)
        output = self.repo / "notes.md"
        command = [sys.executable, str(Path(__file__).with_name("generate_changelog.py")),
                   "--repo", str(self.repo), "--repository", REPO, "--to-tag", "v2.0.0",
                   "--output", str(output)]
        subprocess.run(command, check=True, env={**os.environ, "PYTHONUTF8": "1"})
        first = output.read_bytes()
        subprocess.run(command, check=True)
        self.assertEqual(output.read_bytes(), first)
        text = first.decode("utf-8")
        self.assertTrue(text.startswith("## Alterações importantes\n\n- Corrigir ação ("))
        self.assertIn("  - **Migração:** formato novo\n", text)
        self.assertIn("[v1.0.0...v2.0.0](https://github.com/owner/repo/compare/v1.0.0...v2.0.0)", text)
        self.assertNotIn("Não publicada", text)
        self.assertEqual(read_commits(self.repo, "v2.0.0", "v2.0.0"), [])

    def test_cli_with_assets_writes_checksums_and_downloads(self):
        self.commit("feat: Início")
        git(self.repo, "tag", "v1.0.0")
        assets = self.repo / "assets"
        assets.mkdir()
        (assets / "Zapive-Setup-1.0.0.exe").write_bytes(b"installer")
        output, sums = self.repo / "notes.md", self.repo / "SHA256SUMS.txt"
        subprocess.run([sys.executable, str(Path(__file__).with_name("generate_changelog.py")),
                        "--repo", str(self.repo), "--repository", REPO, "--to-tag", "v1.0.0",
                        "--assets-dir", str(assets), "--checksums-output", str(sums),
                        "--output", str(output)],
                       check=True, env={**os.environ, "PYTHONUTF8": "1"})
        digest = hashlib.sha256(b"installer").hexdigest()
        self.assertEqual(sums.read_text(encoding="utf-8"), digest + "  Zapive-Setup-1.0.0.exe\n")
        text = output.read_text(encoding="utf-8")
        self.assertIn("## " + DOWNLOADS_HEADING, text)
        self.assertIn(digest, text)

    def test_merged_pull_requests_are_attached_to_their_commits(self):
        self.commit("feat: Base")
        git(self.repo, "tag", "v1.0.0")
        git(self.repo, "checkout", "-q", "-b", "topic")
        self.commit("fix(sync): Corrigir fila")
        git(self.repo, "checkout", "-q", "-")
        git(self.repo, "merge", "--no-ff", "-m", "Merge pull request #7 from owner/topic", "topic")
        commits = {c.subject: c for c in read_commits(self.repo, "HEAD", "v1.0.0")}
        self.assertEqual(commits["fix(sync): Corrigir fila"].pr, 7)
        self.assertEqual(commits["Merge pull request #7 from owner/topic"].pr, None)

    def test_first_release_includes_all_history_and_ignores_bump(self):
        self.commit("feat: Início")
        self.commit("Uma melhoria sem prefixo")
        self.commit("chore(release): v0.1.0")
        git(self.repo, "tag", "v0.1.0")
        notes = generate_notes(read_commits(self.repo, "v0.1.0"))
        self.assertIn("Início", notes)
        self.assertIn("Uma melhoria sem prefixo", notes)
        self.assertNotIn("v0.1.0", notes)

    def test_invalid_tag_fails(self):
        self.commit("feat: Início")
        with self.assertRaises(subprocess.CalledProcessError):
            read_commits(self.repo, "missing")
        with self.assertRaises(subprocess.CalledProcessError):
            read_commits(self.repo, "HEAD", "missing")


if __name__ == "__main__":
    unittest.main()
