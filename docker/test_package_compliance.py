import hashlib
import importlib.util
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "compliance", Path(__file__).with_name("package-compliance.py")
)
compliance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compliance)


class PackageComplianceTests(unittest.TestCase):
    def test_fn_dsa_notices_cover_all_five_exact_published_packages(self):
        source = Path("/source")
        for name in ["fn-dsa", "fn-dsa-comm", "fn-dsa-kgen", "fn-dsa-sign", "fn-dsa-vrfy"]:
            item = {"name": name, "version": "0.4.0", "license": "Unlicense",
                    "source": "registry+https://github.com/rust-lang/crates.io-index"}
            notices = compliance.supplemental_notices(item, source, None)
            self.assertEqual([name for _, name in notices], ["LICENSE-FN-DSA", "NOTICE-FN-DSA"])
            self.assertEqual(compliance.supplemental_notices(dict(item, version="0.5.0"), source, None), [])
            self.assertEqual(compliance.supplemental_notices(dict(item, source="git+other"), source, None), [])

    def test_portfu_owner_grant_is_scoped_and_ships_complete_license(self):
        revision = "8cebf34added647488756b15fbeae3c608ab75f5"
        item = {
            "id": "pinned-portfu", "name": "portfu", "version": "2.0.0",
            "source": f"git+https://github.com/GalactechsLLC/portfu.git?rev={revision}#{revision}",
            "license": None, "license_file": None,
        }
        for name in ["portfu", "portfu_common", "portfu_macros"]:
            self.assertEqual(compliance.owner_license(dict(item, name=name)), "Apache-2.0")
        for key, value in [
            ("name", "different-package"), ("version", "2.0.1"),
            ("source", item["source"] + "different-revision"),
            ("license", "MIT"), ("license_file", "LICENSE"),
        ]:
            self.assertIsNone(compliance.owner_license(dict(item, **{key: value})))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "export"
            (source / "licenses").mkdir(parents=True)
            (source / "licenses" / "NOTICE-DISTRIBUTION").write_text("distribution notice")
            (source / "licenses" / "NOTICE-PORTFU").write_text("owner confirmation")
            apache = (Path(__file__).parent.parent / "LICENSE").read_text()
            (source / "LICENSE").write_text(apache)
            dependency = root / "dependency"
            dependency.mkdir()
            item["manifest_path"] = str(dependency / "Cargo.toml")

            def fake_command(*args, cwd):
                if args[:2] == ("cargo", "vendor"):
                    Path(args[-1]).mkdir()
                    return ""
                if args[:2] == ("cargo", "metadata"):
                    return json.dumps({"packages": [item]})
                return "test toolchain"

            output = root / "bundle"
            with patch.object(compliance, "command", side_effect=fake_command):
                compliance.package(source, output, [])
            index = json.loads((output / "licenses" / "DEPENDENCIES.json").read_text())
            self.assertEqual(index[0]["license"], "Apache-2.0")
            self.assertIsNone(index[0]["declared_license"])
            self.assertIn("Owner confirmation", index[0]["license_basis"])
            texts = [(output / "licenses" / p).read_text() for p in index[0]["notice_files"]]
            self.assertIn(apache, texts)
            self.assertIn("owner confirmation", texts)

    def test_sources_notices_and_offline_configuration_travel_together(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "export"
            source.mkdir()
            (source / "licenses").mkdir()
            (source / "licenses" / "NOTICE-DISTRIBUTION").write_text("distribution notice")
            (source / ".cargo").mkdir()
            (source / ".cargo" / "config.toml").write_text('[build]\nrustflags = ["-g"]')
            (source / "main.rs").write_text("fn main() {}")
            (source / "alias.rs").symlink_to("main.rs")
            (source / "sources.private.json").write_text("private")
            for secret in [".env", "node.key", "client.pem", "falcon-keys.bin", "credentials.toml", "wallet.sqlite"]:
                (source / secret).write_text("synthetic secret fixture")
            (source / "wallet-data").mkdir()
            (source / "wallet-data" / "wallet.json").write_text("synthetic wallet fixture")
            (source / "target").mkdir()
            (source / "target" / "binary").write_text("not source")
            dependency = root / "registry" / "example-1.0.0"
            dependency.mkdir(parents=True)
            (dependency / "LICENSE-LGPL.md").write_text("library license")
            (dependency / "LICENSE-GPL.md").write_text("GPL license")
            (dependency / "public-test-fixture.pem").write_text("upstream public fixture")
            (dependency / "lib.rs").write_text("// original copyright\npub fn library() {}")
            package = {
                "id": "registry+example#example@1.0.0", "name": "example",
                "version": "1.0.0", "license": "LGPL-3.0-or-later",
                "license_file": None, "manifest_path": str(dependency / "Cargo.toml"),
                "source": "registry+example",
            }

            def fake_command(*args, cwd):
                if args[0] == "cargo" and args[1] in ("vendor", "metadata"):
                    self.assertIn("--locked", args)
                if args[:2] == ("cargo", "vendor"):
                    vendor = Path(args[-1])
                    compliance.shutil.copytree(dependency, vendor / "example-1.0.0")
                    return f'[source.crates-io]\nreplace-with = "vendored-sources"\n[source.vendored-sources]\ndirectory = "{vendor}"\n'
                if args[:2] == ("cargo", "metadata"):
                    self.assertIn("--all-features", args)
                    return json.dumps({"packages": [package]})
                return "test toolchain"

            output = root / "bundle"
            with patch.object(compliance, "command", side_effect=fake_command):
                compliance.package(source, output, [])
            index = json.loads((output / "licenses" / "DEPENDENCIES.json").read_text())
            self.assertEqual(len(index), 1)
            self.assertEqual(len(index[0]["notice_files"]), 2)
            for notice in index[0]["notice_files"]:
                self.assertTrue((output / "licenses" / notice).exists())
            archive = output / "corresponding-source.tar.gz"
            with tarfile.open(archive) as tar:
                names = tar.getnames()
                self.assertIn("dgx-source/main.rs", names)
                self.assertTrue(tar.getmember("dgx-source/alias.rs").issym())
                self.assertIn("dgx-source/vendor/example-1.0.0/public-test-fixture.pem", names)
                self.assertIn("dgx-source/vendor/example-1.0.0/lib.rs", names)
                self.assertNotIn("dgx-source/target", names)
                self.assertNotIn("dgx-source/sources.private.json", names)
                for secret in [".env", "node.key", "client.pem", "falcon-keys.bin", "credentials.toml", "wallet.sqlite", "wallet-data"]:
                    self.assertNotIn("dgx-source/" + secret, names)
                config = tar.extractfile("dgx-source/.cargo/config.toml").read().decode()
                self.assertIn('rustflags = ["-g"]', config)
                self.assertIn('directory = "vendor"', config)
                self.assertNotIn(str(root), config)
            self.assertEqual(
                (output / "corresponding-source.tar.gz.sha256").read_text().split()[0],
                hashlib.sha256(archive.read_bytes()).hexdigest(),
            )

    def test_rejects_external_links_and_links_to_omitted_secrets_before_cargo(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "export"
            source.mkdir()
            secret = root / "private"
            secret.write_text("synthetic private fixture")
            link = source / "LICENSE"
            link.symlink_to(secret)
            with patch.object(compliance, "command") as command:
                with self.assertRaisesRegex(ValueError, "escapes archive"):
                    compliance.package(source, root / "bundle", [])
                command.assert_not_called()
            link.unlink()
            (source / ".env").write_text("synthetic private fixture")
            link.symlink_to(".env")
            with self.assertRaisesRegex(ValueError, "excluded data"):
                compliance.package(source, root / "bundle", [])

    def test_validates_vendor_links_even_in_runtime_named_directories(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            snapshot = root / "snapshot"
            fixture = snapshot / "vendor" / "example" / "ssl"
            fixture.mkdir(parents=True)
            (root / "private").write_text("synthetic private fixture")
            (fixture / "sample.pem").symlink_to(root / "private")
            with self.assertRaisesRegex(ValueError, "escapes archive"):
                compliance.validate_tree(snapshot, filtered=False)

    def test_rejects_external_sync_manifest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "export"
            source.mkdir()
            (root / "Cargo.toml").write_text("external manifest")
            with patch.object(compliance, "command") as command:
                with self.assertRaisesRegex(ValueError, "inside the source tree"):
                    compliance.package(source, root / "bundle", ["../Cargo.toml"])
                command.assert_not_called()

    def test_rejects_dependency_notice_path_traversal(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "export"
            source.mkdir()
            dependency = root / "dependency"
            dependency.mkdir()
            (root / "private").write_text("synthetic private fixture")
            metadata = {"packages": [{
                "id": "registry+example#example@1", "name": "example", "version": "1",
                "license": None, "license_file": "../private",
                "manifest_path": str(dependency / "Cargo.toml"), "source": "registry+example",
            }]}
            def fake_command(*args, cwd):
                return json.dumps(metadata) if args[:2] == ("cargo", "metadata") else ""
            with patch.object(compliance, "command", side_effect=fake_command):
                with self.assertRaisesRegex(ValueError, "Notice escapes"):
                    compliance.package(source, root / "bundle", [])
            self.assertFalse((root / "bundle" / "corresponding-source.tar.gz").exists())

    def test_refuses_unfiltered_working_tree(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / ".git").mkdir()
            with self.assertRaisesRegex(ValueError, "clean source export"):
                compliance.package(root, root.parent / "unused-output", [])


if __name__ == "__main__":
    unittest.main()
