import contextlib
import json
import logging
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile
import unicodedata
import uuid
import zipfile

MAX_TEXT = 16000
MAX_REQUEST = 128 * 1024
MAX_CATALOG = 8 * 1024 * 1024
MAX_DOWNLOAD = 512 * 1024 * 1024
MAX_UNPACKED = 2 * 1024 * 1024 * 1024
CODE = re.compile(r"[a-z]{2,3}(?:-[a-z]{2,8})?\Z")


class WorkerError(Exception):
    pass


def local_path(value):
    if not isinstance(value, str) or not value or "\x00" in value:
        raise WorkerError("invalid_path")
    if os.name == "nt":
        value = value.replace("/", "\\")
        if value.startswith("\\\\?\\"):
            value = value[4:]
        if not re.match(r"[a-zA-Z]:\\", value):
            raise WorkerError("invalid_path")
        if any(part.endswith((".", " ")) or ":" in part for part in value[3:].split("\\")):
            raise WorkerError("invalid_path")
    path = Path(value)
    if not path.is_absolute() or ".." in path.parts:
        raise WorkerError("invalid_path")
    return path


def plan_route(source, target, available, installed=()):
    installed = set(installed)
    edges = set(available) | installed
    if (source, target) in edges:
        return [(source, target)]
    routes = [[(source, pivot), (pivot, target)]
              for pivot in sorted({end for start, end in edges if start == source} - {source, target})
              if (pivot, target) in edges]
    return min(routes, key=lambda route: (sum(edge not in installed for edge in route),
                                         route[0][1] != "en", route[0][1]), default=None)


class ArgosBackend:
    def __init__(self):
        models = os.environ.get("ARGOS_PACKAGES_DIR")
        if not models or not Path(models).is_absolute():
            raise WorkerError("models_directory_required")
        self.models = Path(models).resolve()
        self.models.mkdir(parents=True, exist_ok=True)
        os.environ.update(ARGOS_DEVICE_TYPE="cpu", ARGOS_DEBUG="0", ARGOS_MODEL_PROVIDER="OPENNMT")
        from argostranslate import package, settings
        self.package = package
        self.settings = settings
        settings.device = "cpu"
        settings.debug = False
        settings.package_dirs = [self.models]
        settings.package_data_dir = self.models
        self.catalog = []
        self.engine = None
        self.engine_path = None
        self.detector = None
        self.catalog_path = self.models.parent / "translation-index.json"
        if self.catalog_path.is_file():
            with self.catalog_path.open("rb") as catalog:
                self.catalog = self.parse_catalog(catalog.read(MAX_CATALOG + 1))

    def parse_catalog(self, data):
        if len(data) > MAX_CATALOG:
            raise WorkerError("download_too_large")
        try:
            entries = json.loads(data)
            if not isinstance(entries, list):
                raise ValueError()
            packages = []
            for entry in entries:
                if not isinstance(entry, dict):
                    raise ValueError()
                if entry.get("type", "translate") != "translate":
                    continue
                if not all(isinstance(entry.get(key), str) and CODE.fullmatch(entry[key])
                           for key in ("from_code", "to_code")):
                    raise ValueError()
                packages.append(self.package.AvailablePackage(entry))
            return packages
        except Exception:
            raise WorkerError("invalid_catalog") from None

    @contextlib.contextmanager
    def local_file(self, value, limit):
        path = local_path(value)
        root = local_path(str(self.models.parent / "downloads"))
        try:
            if any(p.is_symlink() or (hasattr(p, "is_junction") and p.is_junction())
                   for p in (path, *path.parents)):
                raise WorkerError("invalid_path")
            resolved = local_path(str(path.resolve(strict=True)))
            root = local_path(str(root.resolve(strict=True)))
            resolved.relative_to(root)
            if resolved == root or not resolved.is_file():
                raise WorkerError("invalid_path")
            stream = resolved.open("rb")
        except (OSError, ValueError):
            raise WorkerError("invalid_path") from None
        with stream:
            if os.fstat(stream.fileno()).st_size > limit:
                raise WorkerError("download_too_large")
            yield stream

    def installed(self):
        return [p for p in self.package.get_installed_packages()
                if p.type == "translate" and (p.package_path / "model" / "model.bin").is_file()
                and hasattr(p, "tokenizer")]

    def status(self):
        def pairs(packages):
            return ",".join(sorted({f"{p.from_code}:{p.to_code}" for p in packages}))
        return {"version": 2, "models": pairs(self.installed()), "pairs": pairs(self.catalog),
                "catalog_url": self.settings.remote_package_index, "catalog_path": str(self.catalog_path)}

    def import_catalog(self, path):
        with self.local_file(path, MAX_CATALOG) as stream:
            data = stream.read(MAX_CATALOG + 1)
        self.parse_catalog(data)
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=self.catalog_path.parent, prefix=".catalog-", delete=False) as stream:
                temporary = Path(stream.name)
                stream.write(data)
            temporary.replace(self.catalog_path)
            with self.catalog_path.open("rb") as stream:
                self.catalog = self.parse_catalog(stream.read(MAX_CATALOG + 1))
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
        return self.status()

    def install_local(self, path, source, target):
        with self.local_file(path, MAX_DOWNLOAD) as stream:
            return self._install_archive(stream, source, target)

    def _install_archive(self, stream, source, target):
        staging = Path(tempfile.mkdtemp(prefix=".translation-", dir=self.models.parent))
        try:
            with zipfile.ZipFile(stream) as archive:
                entries = archive.infolist()
                if len(entries) > 10000 or sum(e.file_size for e in entries) > MAX_UNPACKED:
                    raise WorkerError("invalid_model_archive")
                for entry in entries:
                    path = Path(entry.filename)
                    if (path.is_absolute() or ".." in path.parts or "\\" in entry.filename
                            or ":" in entry.filename or (entry.external_attr >> 16) & 0o170000 == 0o120000):
                        raise WorkerError("invalid_model_archive")
                archive.extractall(staging)
            roots = list(staging.iterdir())
            if len(roots) != 1 or not roots[0].is_dir():
                raise WorkerError("invalid_model_archive")
            model = self.package.Package(roots[0])
            if (model.from_code != source or model.to_code != target or model.type != "translate"
                    or not (roots[0] / "model" / "model.bin").is_file()
                    or not hasattr(model, "tokenizer")):
                raise WorkerError("invalid_model_archive")
            roots[0].rename(self.models / f"{source}_{target}_{uuid.uuid4().hex}")
        except (zipfile.BadZipFile, ValueError, KeyError, AttributeError, OSError, RuntimeError):
            raise WorkerError("invalid_model_archive") from None
        finally:
            shutil.rmtree(staging, ignore_errors=True)
        self.engine = None
        self.engine_path = None
        return self.status()

    def detect(self, text):
        letters = [c for c in text if c.isalpha()]
        scripts = {unicodedata.name(c, "").split(" ")[0] for c in letters}
        # Kana disambiguates Japanese from shared Han characters. Do not guess
        # from mixed scripts or scripts shared by several other languages.
        if len(letters) >= 2:
            if scripts <= {"HIRAGANA", "KATAKANA", "CJK"} and scripts & {"HIRAGANA", "KATAKANA"}:
                return "ja"
            for script, code in (("CJK", "zh"), ("HANGUL", "ko"), ("GREEK", "el"),
                                 ("ARMENIAN", "hy"), ("GEORGIAN", "ka"), ("THAI", "th")):
                if scripts == {script}:
                    return code
        short = len(letters) < 12
        if len(letters) < 6 or (short and len(re.findall(r"[^\W\d_]+", text)) < 2):
            raise WorkerError("source_required")
        if self.detector is None:
            from lingua import LanguageDetectorBuilder
            self.detector = LanguageDetectorBuilder.from_all_languages().with_low_accuracy_mode().build()
        scores = self.detector.compute_language_confidence_values(text)
        confidence, margin = (0.9, 0.3) if short else (0.7, 0.2)
        if (not scores or scores[0].value < confidence
                or (len(scores) > 1 and scores[0].value - scores[1].value < margin)):
            raise WorkerError("source_required")
        code = scores[0].language.iso_code_639_1.name.lower()
        return code

    def translate(self, source, target, text):
        if source in ("", "auto"):
            source = self.detect(text)
        if source == target:
            return {"text": text, "source": source, "target": target}
        models = {(p.from_code, p.to_code): p for p in self.installed()}
        route = plan_route(source, target, models)
        if route is None:
            raise WorkerError("model_not_installed")
        for edge in route:
            text = self._translate_pair(models[edge], text)
        return {"text": text, "source": source, "target": target}

    def _translate_pair(self, model, text):
        if self.engine_path != model.package_path:
            import ctranslate2
            self.engine = ctranslate2.Translator(str(model.package_path / "model"), device="cpu",
                                                compute_type="int8", inter_threads=1, intra_threads=2)
            self.engine_path = model.package_path
        paragraphs = []
        for paragraph in text.split("\n"):
            if not paragraph.strip():
                paragraphs.append(paragraph)
                continue
            sentences = re.split(r"(?<=[.!?。！？])\s+", paragraph)
            chunks = [sentence[i:i + 400] for sentence in sentences for i in range(0, len(sentence), 400)]
            tokenized = [model.tokenizer.encode(chunk) for chunk in chunks]
            prefix = [[model.target_prefix]] * len(tokenized) if model.target_prefix else None
            batches = self.engine.translate_batch(tokenized, target_prefix=prefix, beam_size=4,
                                                   replace_unknowns=True, max_batch_size=32,
                                                   max_input_length=0, max_decoding_length=1024)
            translated = []
            for batch in batches:
                value = model.tokenizer.decode(batch.hypotheses[0])
                if model.target_prefix and value.startswith(model.target_prefix):
                    value = value[len(model.target_prefix):]
                translated.append(value.strip())
            paragraphs.append(" ".join(translated))
        return "\n".join(paragraphs)


class Worker:
    def __init__(self, backend_factory=ArgosBackend):
        self.backend_factory = backend_factory
        self.backend = None

    def dispatch(self, request):
        request_id = 0
        try:
            if not isinstance(request, dict):
                raise WorkerError("invalid_request")
            value = request.get("id")
            if type(value) is not int or not 0 <= value <= 2**64 - 1:
                raise WorkerError("invalid_id")
            request_id = value
            op = request.get("op")
            if op in ("refresh", "install"):
                raise WorkerError("network_managed_by_app")
            if op not in ("status", "import_catalog", "install_local", "translate"):
                raise WorkerError("invalid_operation")
            source, target, text = (request.get(key, "") for key in ("source", "target", "text"))
            if not all(isinstance(v, str) for v in (source, target, text)):
                raise WorkerError("invalid_request")
            if len(text) > MAX_TEXT or len(source) > 16 or len(target) > 16:
                raise WorkerError("request_too_large")
            if op in ("install_local", "translate"):
                if not CODE.fullmatch(target) or (source not in ("", "auto") and not CODE.fullmatch(source)):
                    raise WorkerError("invalid_language")
                if op == "install_local" and (source in ("", "auto") or source == target):
                    raise WorkerError("invalid_language")
                if op == "translate" and not text.strip():
                    raise WorkerError("empty_text")
            if self.backend is None:
                self.backend = self.backend_factory()
            if op == "translate":
                result = self.backend.translate(source, target, text)
            elif op == "install_local":
                result = self.backend.install_local(request.get("path"), source, target)
            elif op == "import_catalog":
                result = self.backend.import_catalog(request.get("path"))
            else:
                result = getattr(self.backend, op)()
            return {"id": request_id, "ok": True, "error": "", "result": result}
        except WorkerError as error:
            return {"id": request_id, "ok": False, "error": str(error), "result": {}}
        except Exception:
            return {"id": request_id, "ok": False, "error": "runtime_failed", "result": {}}


def serve(input_stream, output_stream, worker):
    while True:
        line = input_stream.readline(MAX_REQUEST + 1)
        if not line:
            return
        if len(line) > MAX_REQUEST:
            while line and not line.endswith(b"\n"):
                line = input_stream.readline(MAX_REQUEST + 1)
            response = {"id": 0, "ok": False, "error": "request_too_large", "result": {}}
        else:
            try:
                request = json.loads(line.decode("utf-8"))
                response = worker.dispatch(request)
            except (ValueError, UnicodeError, RecursionError):
                response = {"id": 0, "ok": False, "error": "invalid_json", "result": {}}
        output_stream.write(json.dumps(response, ensure_ascii=True, separators=(",", ":")) + "\n")
        output_stream.flush()


def main():
    protocol = os.fdopen(os.dup(sys.stdout.fileno()), "w", encoding="utf-8", buffering=1)
    with open(os.devnull, "w") as null:
        os.dup2(null.fileno(), 1)
        os.dup2(null.fileno(), 2)
        logging.disable(logging.CRITICAL)
        with contextlib.redirect_stdout(null), contextlib.redirect_stderr(null):
            serve(sys.stdin.buffer, protocol, Worker())


if __name__ == "__main__":
    main()
