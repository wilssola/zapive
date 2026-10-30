import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from generate_changelog import generate_notes, git, read_commits


class GenerateNotesTests(unittest.TestCase):
    def test_categories_and_prefix_removal(self):
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
            "## Novidades\n\n- Adicionar busca\n\n"
            "## Melhorias\n\n- Reduzir latência\n- Simplificar navegação\n\n"
            "## Correções\n\n- Corrigir sincronização (#12)\n\n"
            "## Segurança\n\n- Validar origem\n\n"
            "## Alterações importantes\n\n- Remover API antiga\n\n"
            "## Outras alterações\n\n- Atualizar janela principal\n- Ajustar janela\n"
        ))

    def test_maintenance_and_version_bumps_are_ignored(self):
        commits = [(kind + "(release): v1.2.3", "") for kind in
                   ("chore", "ci", "build", "test", "docs", "style", "bump")]
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
        self.assertNotIn("contrato removido", notes)
        self.assertIn("- Mensagem livre\n", notes)
        self.assertEqual(notes.count("\n- "), 4)

    def test_deduplication_and_breaking_priority(self):
        notes = generate_notes([
            ("feat(ui): Melhorar busca", ""),
            ("fix: melhorar   busca", ""),
            ("chore!: MELHORAR BUSCA", ""),
            ("feat: Melhorar busca", ""),
        ])
        self.assertEqual(notes, "## Alterações importantes\n\n- Melhorar busca\n")

    def test_nonconventional_subjects_are_preserved(self):
        subjects = ["Melhorar interface: busca", "fix:sem espaço", "Bump version to 2", "Merge branch 'topic'"]
        notes = generate_notes([(subject, "Texto que não deve aparecer") for subject in subjects])
        self.assertEqual(notes, "## Outras alterações\n\n" + "\n".join("- " + s for s in subjects) + "\n")


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

    def test_first_release_with_single_root_commit(self):
        self.commit("feat: Primeira função")
        git(self.repo, "tag", "v1.0.0")
        self.assertEqual(read_commits(self.repo, "v1.0.0"), [("feat: Primeira função", "")])

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
                   "--repo", str(self.repo), "--to-tag", "v2.0.0", "--output", str(output)]
        subprocess.run(command, check=True, env={**os.environ, "PYTHONUTF8": "1"})
        first = output.read_bytes()
        subprocess.run(command, check=True)
        self.assertEqual(output.read_bytes(), first)
        self.assertEqual(first.decode("utf-8"), "## Alterações importantes\n\n- Corrigir ação\n")
        self.assertEqual(read_commits(self.repo, "v2.0.0", "v2.0.0"), [])

    def test_first_release_includes_all_history_and_ignores_bump(self):
        self.commit("feat: Início")
        self.commit("Uma melhoria sem prefixo")
        self.commit("chore(release): v0.1.0")
        git(self.repo, "tag", "v0.1.0")
        notes = generate_notes(read_commits(self.repo, "v0.1.0"))
        self.assertIn("- Início\n", notes)
        self.assertIn("- Uma melhoria sem prefixo\n", notes)
        self.assertNotIn("v0.1.0", notes)

    def test_invalid_tag_fails(self):
        self.commit("feat: Início")
        with self.assertRaises(subprocess.CalledProcessError):
            read_commits(self.repo, "missing")
        with self.assertRaises(subprocess.CalledProcessError):
            read_commits(self.repo, "HEAD", "missing")


if __name__ == "__main__":
    unittest.main()
