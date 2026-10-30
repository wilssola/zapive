import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import venv


ROOT = Path(__file__).resolve().parents[2]


def run(*args):
    subprocess.run([str(arg) for arg in args], cwd=ROOT, check=True)


def main():
    if sys.version_info[:2] != (3, 12):
        raise SystemExit("Build the translation runtime with Python 3.12")
    build = ROOT / "build" / "translation"
    environment = build / "venv"
    venv.EnvBuilder(with_pip=True, clear=True).create(environment)
    python = environment / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    run(python, "-m", "pip", "install", "--no-deps", "-r", ROOT / "packaging/translation/requirements.txt")
    run(python, "-m", "unittest", "discover", "-s", "packaging/translation", "-p", "test_*.py")
    args = [python, "-m", "PyInstaller", "--noconfirm", "--clean", "--onedir", "--console",
            "--name", "zapive-translate", "--distpath", build / "dist", "--workpath", build / "work",
            "--specpath", build, "--collect-all", "lingua", "--collect-all", "ctranslate2",
            "--collect-all", "sentencepiece", "--collect-all", "sacremoses",
            "--copy-metadata", "argostranslate", "--exclude-module", "argostranslate.translate",
            "--exclude-module", "argostranslate.sbd", "--exclude-module", "torch",
            "--exclude-module", "stanza", "--exclude-module", "spacy", "--exclude-module", "minisbd"]
    if os.name == "nt":
        args += ["--hide-console", "hide-early"]
    args += [ROOT / "packaging/translation/worker.py"]
    run(*args)
    destination = ROOT / "target/release/translation"
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(build / "dist/zapive-translate", destination, symlinks=True)
    exe = destination / ("zapive-translate.exe" if os.name == "nt" else "zapive-translate")
    with tempfile.TemporaryDirectory() as temporary:
        env = dict(os.environ, ARGOS_PACKAGES_DIR=str(Path(temporary) / "models"))
        requests = [{"id": 1, "op": "status"},
                    {"id": 2, "op": "translate", "source": "en", "target": "en", "text": "Offline test"},
                    {"id": 3, "op": "translate", "source": "auto", "target": "pt",
                     "text": "This is a sufficiently long English sentence for language detection."}]
        result = subprocess.run([str(exe)], input="".join(json.dumps(r) + "\n" for r in requests),
                                text=True, encoding="utf-8", capture_output=True, env=env, timeout=120, check=True)
        responses = [json.loads(line) for line in result.stdout.splitlines()]
        if (len(responses) != 3 or not responses[0]["ok"] or not responses[1]["ok"]
                or responses[0]["result"].get("version", 0) < 2
                or responses[0]["result"].get("models") != "" or responses[0]["result"].get("pairs") != ""
                or not isinstance(responses[0]["result"].get("catalog_url"), str)
                or not responses[0]["result"]["catalog_url"].startswith("https://")
                or responses[0]["result"].get("catalog_path") != str(Path(temporary) / "translation-index.json")
                or responses[1]["result"]["text"] != "Offline test"
                or responses[2]["error"] not in ("model_not_installed", "source_required") or result.stderr):
            raise SystemExit("Frozen translation runtime smoke test failed")


if __name__ == "__main__":
    main()
