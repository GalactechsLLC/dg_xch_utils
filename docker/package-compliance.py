#!/usr/bin/env python3
"""Ship the exact Cargo sources and notices alongside container executables."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile


def command(*args, cwd):
    return subprocess.check_output(args, cwd=cwd, text=True)


def excluded(name):
    lower = name.lower()
    return lower in {
        "target", ".git", ".idea", ".agents", ".codex", ".aws", ".ssh",
        "__pycache__", "node_modules", "sources.private.json", "ssl",
        "local-node", "wallet-data", "crawler-data", "config.yaml", "config.yml",
        "credentials", "credentials.json", "credentials.toml", "falcon-keys.bin",
        "id_rsa", "id_ed25519", "id_ecdsa", ".env",
    } or lower.startswith(".env.") or lower.endswith((
        ".key", ".pem", ".p12", ".pfx", ".plot", ".db",
    )) or ".sqlite" in lower or ".db-" in lower


def ignore(directory, names):
    # Also enforce the context's secret exclusions for non-Docker source exports.
    return [name for name in names if excluded(name)]


def inside(path, root):
    return path == root or root in path.parents


def validate_tree(root, filtered=True):
    # Preserve internal source symlinks, but never publish links outside the archive
    # or to omitted credentials. Do not let copytree read FIFOs/devices either.
    for directory, dirs, files in os.walk(root, followlinks=False):
        dirs[:] = [name for name in dirs if not filtered or not excluded(name)]
        for name in dirs + [name for name in files if not filtered or not excluded(name)]:
            path = Path(directory) / name
            resolved = path.resolve()
            if not inside(resolved, root):
                raise ValueError(f"Source path escapes archive: {path}")
            if filtered and any(excluded(part) for part in resolved.relative_to(root).parts):
                raise ValueError(f"Source link targets excluded data: {path}")
            if not path.is_dir() and not path.is_file():
                raise ValueError(f"Source path is not a regular file or directory: {path}")


def notice_files(root):
    prefixes = ("license", "licence", "copying", "copyright", "notice", "authors")
    for directory, dirs, files in os.walk(root, followlinks=False):
        dirs[:] = sorted(name for name in dirs if not excluded(name))
        for name in sorted(files):
            if not excluded(name) and name.lower().startswith(prefixes):
                yield Path(directory) / name


def owner_license(item):
    """Apply only the repository owner's grant for the three exact pinned crates."""
    revision = "8cebf34added647488756b15fbeae3c608ab75f5"
    source = f"git+https://github.com/GalactechsLLC/portfu.git?rev={revision}#{revision}"
    if (
        item["source"] == source
        and item["name"] in {"portfu", "portfu_common", "portfu_macros"}
        and item["version"] == "2.0.0"
        and not item["license"]
        and not item["license_file"]
    ):
        return "Apache-2.0"
    return None


def supplemental_notices(item, source, granted_license):
    if granted_license:
        return [
            (source / "LICENSE", "LICENSE-APACHE-2.0"),
            (source / "licenses" / "NOTICE-PORTFU", "NOTICE-PORTFU"),
        ]
    if (
        item["source"] == "registry+https://github.com/rust-lang/crates.io-index"
        and item["name"] in {"fn-dsa", "fn-dsa-comm", "fn-dsa-kgen", "fn-dsa-sign", "fn-dsa-vrfy"}
        and item["version"] == "0.4.0"
        and item["license"] == "Unlicense"
    ):
        return [(source / "licenses" / name, name) for name in ["LICENSE-FN-DSA", "NOTICE-FN-DSA"]]
    return []


def package(source, output, manifests):
    source = source.resolve()
    output = output.resolve()
    if (source / ".git").exists():
        raise ValueError("Use a clean source export or Docker context, not a Git working tree")
    if output == source or source in output.parents:
        raise ValueError("Output must be outside the source tree")
    for manifest in manifests:
        path = (source / manifest).resolve()
        if not inside(path, source) or not path.is_file():
            raise ValueError(f"Sync manifest must be a file inside the source tree: {manifest}")
    validate_tree(source)
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="dgx-corresponding-source-") as temporary:
        snapshot = Path(temporary) / "dgx-source"
        shutil.copytree(source, snapshot, ignore=ignore, symlinks=True)
        vendor = snapshot / "vendor"
        args = ["cargo", "vendor", "--locked", "--versioned-dirs"]
        for manifest in manifests:
            args.extend(["--sync", str(source / manifest)])
        vendor_config = command(*args, str(vendor), cwd=source)
        config = snapshot / ".cargo" / "config.toml"
        config.parent.mkdir(exist_ok=True)
        with config.open("a") as stream:
            stream.write("\n" + vendor_config.replace(str(vendor), "vendor"))
        packages = {}
        for manifest in ["Cargo.toml", *manifests]:
            metadata = json.loads(command(
                "cargo", "metadata", "--locked", "--all-features", "--format-version", "1",
                "--manifest-path", str(source / manifest), cwd=source,
            ))
            for item in metadata["packages"]:
                packages[item["id"]] = item
        index = []
        for item in sorted(packages.values(), key=lambda p: p["id"]):
            granted_license = owner_license(item)
            if not item["license"] and not item["license_file"] and not granted_license:
                raise RuntimeError(f"No declared license for {item['id']}")
            root = Path(item["manifest_path"]).parent
            identifier = item["name"] + "-" + item["version"]
            # Different sources can provide packages with identical names/versions.
            identifier += "-" + hashlib.sha256(item["id"].encode()).hexdigest()[:12]
            destination = output / "licenses" / "dependencies" / identifier
            paths = set(notice_files(root))
            if item["license_file"]:
                paths.add(root / item["license_file"])
            recorded = []
            for path in sorted(paths):
                resolved = path.resolve()
                allowed_root = source if inside(root.resolve(), source) else root.resolve()
                if not inside(resolved, allowed_root) or not resolved.is_file():
                    raise ValueError(f"Notice escapes package sources or is not a file: {path}")
                if any(excluded(part) for part in resolved.relative_to(allowed_root).parts):
                    raise ValueError(f"Notice targets excluded data: {path}")
                try:
                    relative = resolved.relative_to(root.resolve())
                except ValueError:
                    relative = Path(path.name)
                target = destination / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, target)
                recorded.append(str(target.relative_to(output / "licenses")))
            supplements = supplemental_notices(item, source, granted_license)
            if supplements:
                destination.mkdir(parents=True, exist_ok=True)
                for original, name in supplements:
                    target = destination / name
                    shutil.copy2(original, target)
                    recorded.append(str(target.relative_to(output / "licenses")))
            index.append({
                "package": item["name"], "version": item["version"],
                "source": item["source"], "license": granted_license or item["license"],
                "declared_license": item["license"],
                "license_basis": "Owner confirmation in NOTICE-PORTFU" if granted_license else "Upstream package declaration",
                "notice_files": recorded,
                "source_notices": "All source headers are preserved in corresponding-source.tar.gz",
            })
        (output / "licenses").mkdir(exist_ok=True)
        (output / "licenses" / "DEPENDENCIES.json").write_text(json.dumps(index, indent=2) + "\n")
        build = {
            "rustc": command("rustc", "--version", "--verbose", cwd=source),
            "cargo": command("cargo", "--version", cwd=source),
            "manifests": ["Cargo.toml", *manifests],
        }
        (snapshot / "BUILD-TOOLCHAIN.json").write_text(json.dumps(build, indent=2) + "\n")
        # Vendored upstream fixtures may legitimately use excluded runtime names;
        # preserve their source contents, but validate every archived link and file.
        validate_tree(snapshot, filtered=False)
        archive = output / "corresponding-source.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(snapshot, arcname="dgx-source")
        digest = hashlib.sha256()
        with archive.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        (output / "corresponding-source.tar.gz.sha256").write_text(f"{digest.hexdigest()}  {archive.name}\n")
        for path in notice_files(source / "licenses"):
            target = output / "licenses" / path.relative_to(source / "licenses")
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sync", action="append", default=[])
    arguments = parser.parse_args()
    package(arguments.source, arguments.output, arguments.sync)
