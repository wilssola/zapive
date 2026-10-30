import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import zipfile

from worker import ArgosBackend, MAX_CATALOG, MAX_DOWNLOAD, MAX_REQUEST, MAX_TEXT, Worker, WorkerError, plan_route, serve


class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.backend = Mock()
        self.backend.status.return_value = {"version": 2, "models": "en:pt", "pairs": "en:pt,pt:en",
                                            "catalog_url": "trusted-default", "catalog_path": str(Path.cwd() / "index.json")}
        self.worker = Worker(lambda: self.backend)

    def request(self, op="status", **fields):
        return self.worker.dispatch(dict(id=42, op=op, source="en", target="pt", text="hello", **fields))

    def test_status(self):
        response = self.request()
        self.assertEqual(response, {"id": 42, "ok": True, "error": "", "result": self.backend.status.return_value})
        self.backend.refresh.assert_not_called()
        self.backend.install.assert_not_called()

    def test_translate(self):
        self.backend.translate.return_value = {"text": "olá", "source": "en", "target": "pt"}
        self.assertTrue(self.request("translate")["ok"])
        self.backend.translate.assert_called_once_with("en", "pt", "hello")
        self.backend.install.assert_not_called()
        self.backend.refresh.assert_not_called()

    def test_invalid_requests(self):
        for request in (None, [], {}, {"id": True}, {"id": -1}, {"id": 2**64},
                        {"id": 1, "op": "unknown"}, {"id": 1, "op": []},
                        {"id": 1, "op": "status", "text": "x" * (MAX_TEXT + 1)},
                        {"id": 1, "op": "translate", "source": "auto", "target": "../en"},
                        {"id": 1, "op": "install_local", "source": "auto", "target": "en"},
                        {"id": 1, "op": "install_local", "source": "en", "target": "en"},
                        {"id": 1, "op": "translate", "source": "en", "target": "pt", "text": " "}):
            with self.subTest(request_type=type(request)):
                self.assertFalse(self.worker.dispatch(request)["ok"])
        self.backend.status.assert_not_called()

    def test_errors_do_not_echo_input(self):
        self.backend.translate.side_effect = RuntimeError("private message")
        response = self.request("translate")
        self.assertEqual(response["error"], "runtime_failed")
        self.assertNotIn("private", json.dumps(response))
        self.backend.translate.side_effect = WorkerError("source_required")
        self.assertEqual(self.request("translate")["error"], "source_required")

    def test_jsonl_recovers_and_preserves_u64(self):
        stream = io.BytesIO(b'bad\n\xff\n' + b'x' * (MAX_REQUEST + 1) + b'\n'
                            + b'{"id":18446744073709551615,"op":"status"}\n')
        output = io.StringIO()
        serve(stream, output, self.worker)
        responses = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual([r["error"] for r in responses], ["invalid_json", "invalid_json", "request_too_large", ""])
        self.assertEqual(responses[-1]["id"], 2**64 - 1)

    def test_short_auto_requires_source(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        with self.assertRaisesRegex(WorkerError, "source_required"):
            backend.detect("hi")

    def test_short_distinctive_scripts(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        for text, code in (("什么时候口整好", "zh"), ("你好", "zh"), ("いつ完成する", "ja"),
                           ("こんにちは", "ja"), ("안녕하세요", "ko"), ("γειά", "el"),
                           ("გამარჯობა", "ka"), ("բարեւ", "hy"), ("สวัสดี", "th")):
            with self.subTest(text=text):
                self.assertEqual(backend.detect(text), code)

    def test_short_ambiguous_scripts_and_words(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        for text in ("hello", "bonjour", "a", "你", "你好 hi", "да", "مرحبا", "123!", ""):
            with self.subTest(text=text), self.assertRaisesRegex(WorkerError, "source_required"):
                backend.detect(text)

    def test_short_sentence_confidence(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        language = SimpleNamespace(iso_code_639_1=SimpleNamespace(name="EN"))
        backend.detector = Mock()
        for values in ([], [0.8, 0.1], [0.95, 0.7]):
            backend.detector.compute_language_confidence_values.return_value = [
                SimpleNamespace(value=v, language=language) for v in values]
            with self.assertRaisesRegex(WorkerError, "source_required"):
                backend.detect("How are you?")
        backend.detector.compute_language_confidence_values.return_value = [
            SimpleNamespace(value=0.96, language=language), SimpleNamespace(value=0.02, language=language)]
        self.assertEqual(backend.detect("How are you?"), "en")

    def test_detection_confidence(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        language = SimpleNamespace(iso_code_639_1=SimpleNamespace(name="EN"))
        backend.detector = Mock()
        for values in ([0.6, 0.1], [0.8, 0.7]):
            backend.detector.compute_language_confidence_values.return_value = [
                SimpleNamespace(value=v, language=language) for v in values]
            with self.assertRaisesRegex(WorkerError, "source_required"):
                backend.detect("This is a sufficiently long sentence")
        backend.detector.compute_language_confidence_values.return_value = [SimpleNamespace(value=0.95, language=language)]
        self.assertEqual(backend.detect("This is a sufficiently long sentence"), "en")

    def test_missing_model_is_offline(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        backend.installed = Mock(return_value=[])
        backend.detector = None
        with patch("socket.socket", side_effect=AssertionError("network forbidden")) as network:
            with self.assertRaisesRegex(WorkerError, "model_not_installed"):
                backend.translate("en", "pt", "hello")
            self.assertEqual(backend.translate("en", "en", "hello")["text"], "hello")
            with self.assertRaisesRegex(WorkerError, "model_not_installed"):
                backend.translate("auto", "pt", "什么时候口整好")
            network.assert_not_called()

    def test_cpu_translation_without_sbd(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        tokenizer = Mock()
        tokenizer.encode.return_value = ["hello"]
        tokenizer.decode.return_value = "olá"
        model = SimpleNamespace(from_code="en", to_code="pt", package_path=Path("model"),
                                tokenizer=tokenizer, target_prefix="")
        backend.installed = Mock(return_value=[model])
        backend.engine_path = None
        engine = Mock()
        engine.translate_batch.return_value = [SimpleNamespace(hypotheses=[["olá"]])]
        module = SimpleNamespace(Translator=Mock(return_value=engine))
        backend.detector = Mock()
        backend.detector.compute_language_confidence_values.return_value = [
            SimpleNamespace(value=0.99, language=SimpleNamespace(iso_code_639_1=SimpleNamespace(name="EN")))]
        with patch.dict("sys.modules", ctranslate2=module), \
                patch("socket.socket", side_effect=AssertionError("network forbidden")) as network:
            for source in ("en", "auto"):
                self.assertEqual(backend.translate(source, "pt", "Hello, how are you?")["text"], "olá")
            network.assert_not_called()
        self.assertEqual(module.Translator.call_args.kwargs["device"], "cpu")

    def test_route_direct_preferred_even_when_pivot_installed(self):
        direct = ("zh", "pt")
        pivot = [("zh", "en"), ("en", "pt")]
        self.assertEqual(plan_route("zh", "pt", [direct], pivot), [direct])

    def test_route_minimizes_downloads_before_english_priority(self):
        english = [("zh", "en"), ("en", "pt")]
        french = [("zh", "fr"), ("fr", "pt")]
        available = english + french
        self.assertEqual(plan_route("zh", "pt", available), english)
        self.assertEqual(plan_route("zh", "pt", available, [french[0]]), french)
        self.assertEqual(plan_route("zh", "pt", available, [french[0], english[0]]), english)
        self.assertEqual(plan_route("zh", "pt", [], french), french)

    def test_route_deterministic_and_bounded(self):
        available = [("zh", "fr"), ("fr", "pt"), ("zh", "de"), ("de", "pt")]
        expected = [("zh", "de"), ("de", "pt")]
        for edges in (available, reversed(available)):
            self.assertEqual(plan_route("zh", "pt", edges), expected)
        for edges in ([], [("pt", "zh")], [("zh", "en"), ("en", "fr"), ("fr", "pt")],
                      [("zh", "en"), ("en", "zh")]):
            self.assertIsNone(plan_route("zh", "pt", edges))

    def test_pivot_translation_is_offline_and_preserves_detected_source(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        models = []
        engines = []
        for source, target, output in (("zh", "en", "hello"), ("en", "pt", "olá")):
            tokenizer = Mock()
            tokenizer.encode.side_effect = lambda text: [text]
            tokenizer.decode.side_effect = lambda tokens: tokens[0]
            models.append(SimpleNamespace(from_code=source, to_code=target, package_path=Path(source + target),
                                          tokenizer=tokenizer, target_prefix=""))
            engine = Mock()
            engine.translate_batch.return_value = [SimpleNamespace(hypotheses=[[output]])]
            engines.append(engine)
        backend.installed = Mock(return_value=models)
        backend.engine_path = None
        backend.install = Mock(side_effect=AssertionError("install forbidden"))
        backend.refresh = Mock(side_effect=AssertionError("refresh forbidden"))
        module = SimpleNamespace(Translator=Mock(side_effect=engines * 2))
        with patch.dict("sys.modules", ctranslate2=module), \
                patch("socket.socket", side_effect=AssertionError("network forbidden")) as network:
            for source in ("zh", "auto"):
                self.assertEqual(backend.translate(source, "pt", "你好"),
                                 {"text": "olá", "source": "zh", "target": "pt"})
            network.assert_not_called()
        self.assertEqual(models[0].tokenizer.encode.call_args.args, ("你好",))
        self.assertEqual(models[1].tokenizer.encode.call_args.args, ("hello",))
        backend.install.assert_not_called()
        backend.refresh.assert_not_called()

    def test_translation_prefers_direct_installed_model(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        models = [SimpleNamespace(from_code=source, to_code=target)
                  for source, target in (("zh", "en"), ("en", "pt"), ("zh", "pt"))]
        backend.installed = Mock(return_value=models)
        backend._translate_pair = Mock(return_value="olá")
        self.assertEqual(backend.translate("zh", "pt", "你好"),
                         {"text": "olá", "source": "zh", "target": "pt"})
        backend._translate_pair.assert_called_once_with(models[-1], "你好")

    def test_translation_missing_second_hop_does_not_translate(self):
        backend = ArgosBackend.__new__(ArgosBackend)
        backend.installed = Mock(return_value=[SimpleNamespace(from_code="zh", to_code="en")])
        backend._translate_pair = Mock()
        with self.assertRaisesRegex(WorkerError, "model_not_installed"):
            backend.translate("zh", "pt", "你好")
        backend._translate_pair.assert_not_called()


class LocalFileTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.downloads = self.root / "downloads"
        self.downloads.mkdir()
        self.backend = ArgosBackend.__new__(ArgosBackend)
        self.backend.models = self.root / "models"
        self.backend.models.mkdir()
        self.backend.catalog_path = self.root / "translation-index.json"
        self.backend.catalog = []
        self.backend.settings = SimpleNamespace(remote_package_index="trusted-default")
        self.backend.package = SimpleNamespace(
            AvailablePackage=lambda entry: SimpleNamespace(**entry),
            Package=self.package,
            get_installed_packages=lambda: [self.package(p) for p in self.backend.models.iterdir()])
        self.worker = Worker(lambda: self.backend)

    def package(self, path):
        return SimpleNamespace(from_code="en", to_code="pt", type="translate",
                               tokenizer=Mock(), package_path=path)

    def archive(self, names=("en_pt/model/model.bin",)):
        path = self.downloads / "model.argosmodel"
        with zipfile.ZipFile(path, "w") as archive:
            for name in names:
                archive.writestr(name, "weights")
        return path

    def request(self, op, path, **fields):
        return self.worker.dispatch(dict(id=1, op=op, path=str(path), **fields))

    def test_status_contract_and_legacy_operations_are_offline(self):
        with patch("socket.socket", side_effect=AssertionError("network forbidden")) as network:
            result = self.backend.status()
            self.assertEqual(result, {"version": 2, "models": "", "pairs": "",
                                      "catalog_url": "trusted-default",
                                      "catalog_path": str(self.backend.catalog_path)})
            for op in ("refresh", "install"):
                worker = Worker(Mock(side_effect=AssertionError("backend forbidden")))
                self.assertEqual(worker.dispatch(dict(id=1, op=op))["error"], "network_managed_by_app")
            network.assert_not_called()

    def test_import_catalog_atomically_saves_and_reloads(self):
        path = self.downloads / "catalog.json"
        data = b'[{"from_code":"en","to_code":"pt"}]'
        path.write_bytes(data)
        with patch("socket.socket", side_effect=AssertionError("network forbidden")) as network:
            response = self.request("import_catalog", path)
            self.assertTrue(response["ok"])
            self.assertEqual(response["result"]["pairs"], "en:pt")
            network.assert_not_called()
        self.assertEqual(self.backend.catalog_path.read_bytes(), data)
        self.backend.catalog = []
        self.backend.catalog = self.backend.parse_catalog(self.backend.catalog_path.read_bytes())
        self.assertEqual(self.backend.status()["pairs"], "en:pt")
        self.assertEqual(list(self.root.glob(".catalog-*")), [])

    def test_invalid_catalog_preserves_previous_catalog(self):
        path = self.downloads / "catalog.json"
        path.write_bytes(b'[{"from_code":"en","to_code":"pt"}]')
        self.backend.import_catalog(str(path))
        original = self.backend.catalog_path.read_bytes()
        for data in (b"bad", b"{}", b"[null]", b'[{"from_code":12,"to_code":"pt"}]',
                     b'[{"from_code":"../en","to_code":"pt"}]', b'[{"from_code":"en"}]'):
            with self.subTest(data=data):
                path.write_bytes(data)
                self.assertEqual(self.request("import_catalog", path)["error"], "invalid_catalog")
                self.assertEqual(self.backend.catalog_path.read_bytes(), original)
                self.assertEqual(self.backend.status()["pairs"], "en:pt")
        path.write_bytes(b"[]")
        with patch.object(Path, "replace", side_effect=OSError("private")):
            self.assertEqual(self.request("import_catalog", path)["error"], "runtime_failed")
        self.assertEqual(self.backend.catalog_path.read_bytes(), original)
        self.assertEqual(list(self.root.glob(".catalog-*")), [])

    def test_file_size_bounds(self):
        for op, limit in (("import_catalog", MAX_CATALOG), ("install_local", MAX_DOWNLOAD)):
            with self.subTest(op=op):
                path = self.downloads / "oversized"
                with path.open("wb") as stream:
                    stream.truncate(limit + 1)
                self.assertEqual(self.request(op, path, source="en", target="pt")["error"], "download_too_large")
                with path.open("wb") as stream:
                    stream.truncate(limit)
                with self.backend.local_file(str(path), limit):
                    pass
        with self.assertRaisesRegex(WorkerError, "download_too_large"):
            self.backend.parse_catalog(b" " * (MAX_CATALOG + 1))

    def test_paths_must_be_absolute_existing_files_inside_downloads(self):
        outside = self.root / "outside.json"
        outside.write_bytes(b"[]")
        sibling = self.root / "downloads-other"
        sibling.mkdir()
        (sibling / "catalog.json").write_bytes(b"[]")
        for path in ("relative.json", self.downloads / "missing", outside, sibling / "catalog.json",
                     self.downloads, self.downloads / ".." / "outside.json", ""):
            for op in ("import_catalog", "install_local"):
                with self.subTest(path=str(path), op=op):
                    self.assertEqual(self.request(op, path, source="en", target="pt")["error"], "invalid_path")
        for path in (None, 123, []):
            self.assertEqual(self.worker.dispatch(dict(id=1, op="import_catalog", path=path))["error"], "invalid_path")

    def test_symlinks_rejected(self):
        target = self.downloads / "catalog.json"
        target.write_bytes(b"[]")
        link = self.downloads / "link.json"
        try:
            link.symlink_to(target)
        except OSError:
            self.skipTest("Symlink creation unavailable")
        self.assertEqual(self.request("import_catalog", link)["error"], "invalid_path")
        directory = self.downloads / "linked"
        directory.symlink_to(self.downloads, target_is_directory=True)
        self.assertEqual(self.request("import_catalog", directory / target.name)["error"], "invalid_path")
        if os.name == "nt":
            for path in (link, directory / target.name):
                with patch.object(Path, "open", side_effect=AssertionError("must not open")):
                    self.assertEqual(self.request("import_catalog", "\\\\?\\" + str(path))["error"], "invalid_path")

    @unittest.skipUnless(os.name == "nt", "Windows paths")
    def test_windows_extended_dos_paths_and_canonical_containment(self):
        path = self.archive()
        models = self.backend.models
        for extended_models in (False, True):
            self.backend.models = Path("\\\\?\\" + str(models)) if extended_models else models
            for value in (str(path), "\\\\?\\" + str(path), ("\\\\?\\" + str(path)).swapcase()):
                with self.subTest(models=str(self.backend.models), value=value):
                    with self.backend.local_file(value, MAX_DOWNLOAD) as stream:
                        self.assertEqual(stream.read(2), b"PK")
        self.backend.models = models
        self.assertTrue(self.request("install_local", "\\\\?\\" + str(path), source="en", target="pt")["ok"])
        catalog = self.downloads / "catalog.json"
        catalog.write_bytes(b"[]")
        self.assertTrue(self.request("import_catalog", "\\\\?\\" + str(catalog))["ok"])

    @unittest.skipUnless(os.name == "nt", "Windows paths")
    def test_windows_extended_escapes_and_namespaces_rejected_without_open(self):
        path = self.archive()
        sibling = self.root / "downloads-other"
        sibling.mkdir()
        outside = sibling / path.name
        outside.write_bytes(b"outside")
        values = ["\\\\?\\" + str(outside), "\\\\?\\" + str(self.downloads / ".." / sibling.name / path.name),
                  "\\\\server\\share\\model", "\\\\?\\UNC\\server\\share\\model", "\\\\.\\C:\\model",
                  "\\\\?\\GLOBALROOT\\Device\\model", "\\\\?\\C:model", str(path) + ":stream",
                  "\\\\?\\" + str(path) + ".", "\\\\?\\" + str(path) + " "]
        with patch.object(Path, "open", side_effect=AssertionError("must not open")):
            for value in values:
                with self.subTest(value=value):
                    self.assertEqual(self.request("import_catalog", value)["error"], "invalid_path")

    def test_all_ancestors_including_root_reject_symlinks_without_open(self):
        path = self.archive()
        values = [str(path)]
        if os.name == "nt":
            values.append("\\\\?\\" + str(path))
        for ancestor in (path, *path.parents):
            for value in values:
                with self.subTest(ancestor=str(ancestor), value=value):
                    with patch.object(Path, "is_symlink", autospec=True,
                                      side_effect=lambda p: p == ancestor), \
                            patch.object(Path, "open", side_effect=AssertionError("must not open")):
                        self.assertEqual(self.request("import_catalog", value)["error"], "invalid_path")

    @unittest.skipUnless(hasattr(Path, "is_junction"), "Junction detection unavailable")
    def test_all_ancestors_including_root_reject_reparse_without_open(self):
        path = self.archive()
        values = [str(path)]
        if os.name == "nt":
            values.append("\\\\?\\" + str(path))
        for ancestor in (path, *path.parents):
            for value in values:
                with self.subTest(ancestor=str(ancestor), value=value):
                    with patch.object(Path, "is_junction", autospec=True,
                                      side_effect=lambda p: p == ancestor), \
                            patch.object(Path, "open", side_effect=AssertionError("must not open")):
                        self.assertEqual(self.request("import_catalog", value)["error"], "invalid_path")

    @unittest.skipUnless(os.name == "nt", "Windows junctions")
    def test_windows_junction_directory_and_ancestor_rejected(self):
        target = self.downloads / "catalog.json"
        target.write_bytes(b"[]")
        junction = self.downloads / "junction"
        result = subprocess.run(["cmd", "/c", "mklink", "/J", str(junction), str(self.downloads)],
                                capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.addCleanup(junction.rmdir)
        for path in (junction / target.name, junction / "junction" / target.name):
            for value in (str(path), "\\\\?\\" + str(path)):
                with self.subTest(value=value), \
                        patch.object(Path, "open", side_effect=AssertionError("must not open")):
                    self.assertEqual(self.request("import_catalog", value)["error"], "invalid_path")

    def test_install_local_publishes_validated_model_without_catalog(self):
        path = self.archive()
        with patch("socket.socket", side_effect=AssertionError("network forbidden")) as network:
            response = self.request("install_local", path, source="en", target="pt")
            self.assertTrue(response["ok"])
            self.assertEqual(response["result"]["models"], "en:pt")
            self.assertEqual(response["result"]["pairs"], "")
            network.assert_not_called()
        self.assertEqual(len(list(self.backend.models.iterdir())), 1)
        self.assertIsNone(self.backend.engine)
        self.assertEqual(list(self.root.glob(".translation-*")), [])

    def test_model_metadata_and_archive_validation(self):
        path = self.archive()
        for fields in (dict(from_code="zh"), dict(to_code="fr"), dict(type="sbd"), dict(tokenizer=None)):
            model = self.package(Path("unused"))
            for key, value in fields.items():
                if key == "tokenizer":
                    del model.tokenizer
                else:
                    setattr(model, key, value)
            with self.subTest(fields=fields), patch.object(self.backend.package, "Package", return_value=model):
                self.assertEqual(self.request("install_local", path, source="en", target="pt")["error"], "invalid_model_archive")
        for names in (("../escape",), ("/absolute",), ("en_pt\\escape",), ("C:escape",),
                      ("a/model/model.bin", "b/model/model.bin"), ("en_pt/no-model",)):
            with self.subTest(names=names):
                path = self.archive(names)
                self.assertEqual(self.request("install_local", path, source="en", target="pt")["error"], "invalid_model_archive")
        path.write_bytes(b"not a zip")
        self.assertEqual(self.request("install_local", path, source="en", target="pt")["error"], "invalid_model_archive")
        self.assertEqual(list(self.backend.models.iterdir()), [])
        self.assertEqual(list(self.root.glob(".translation-*")), [])
        self.assertFalse((self.root / "escape").exists())

    def test_archive_symlink_and_unpacked_limits(self):
        path = self.downloads / "model.argosmodel"
        entry = zipfile.ZipInfo("en_pt/link")
        entry.create_system = 3
        entry.external_attr = 0o120777 << 16
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr(entry, "outside")
        self.assertEqual(self.request("install_local", path, source="en", target="pt")["error"], "invalid_model_archive")
        path = self.archive()
        with patch("worker.MAX_UNPACKED", 1):
            self.assertEqual(self.request("install_local", path, source="en", target="pt")["error"], "invalid_model_archive")
        with patch.object(zipfile.ZipFile, "infolist", return_value=[entry] * 10001):
            self.assertEqual(self.request("install_local", path, source="en", target="pt")["error"], "invalid_model_archive")
        self.assertEqual(list(self.backend.models.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
