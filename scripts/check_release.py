#!/usr/bin/env python3
"""Build and audit the single crates.io archive, including the bundled database engine."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
FORBIDDEN_PACKAGES = {
    "turso", "turso_core", "turso_sdk_kit", "turso_sync_engine", "turso_sync_sdk_kit",
}
VENDOR_DIRECTORIES = {"turso_core", "turso_sdk_kit", "turso"}


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def audit_archives(package_dir):
    """Ensure the one published CLT package contains and builds its own engine."""
    current = tomllib.loads((ROOT / "Cargo.toml").read_text())
    version = current["package"]["version"]
    prefix = f"clt-rs-{version}"
    archive_path = package_dir / f"{prefix}.crate"
    with tarfile.open(archive_path) as archive:
        def read(relative):
            return archive.extractfile(f"{prefix}/{relative}").read()

        manifest = tomllib.loads(read("Cargo.toml").decode())
        require("patch" not in manifest, "Published manifest contains a Cargo patch")
        require(manifest["lib"]["path"] == "vendor/turso_core/lib.rs",
                "CLT must compile the bundled core as its own library target")
        require(manifest["lib"]["name"] == "clt_database", "Unexpected bundled library target")
        require(manifest["lib"]["edition"] == "2021", "Keep the engine's upstream Rust edition")
        lock = tomllib.loads(read("Cargo.lock").decode())
        for package in lock["package"]:
            name = package["name"]
            require(name not in FORBIDDEN_PACKAGES and not name.startswith("clt-turso"),
                    f"Published CLT resolves a separate database package: {name}")
            if name != "clt-rs":
                require(package.get("source", "").startswith("registry+"),
                        f"Published dependency is not from a registry: {name}")
        for name in ("turso_ext", "turso_macros", "turso_parser", "turso_sdk_kit_macros"):
            require(manifest["dependencies"][name]["version"] == "=0.7.2",
                    f"The bundled engine's {name} dependency must remain pinned")
        archived = {Path(member.name).relative_to(prefix).as_posix(): member
                    for member in archive.getmembers() if member.isfile()}
        for name in archived:
            require(not (name.startswith("vendor/") and Path(name).name == "Cargo.toml"),
                    f"Nested Cargo package would omit vendored source: {name}")
        # Check every expected file, so a missing module cannot pass by omission.
        expected = [ROOT / "build.rs"]
        expected.extend((ROOT / "src").rglob("*.rs"))
        expected.extend((ROOT / "tests").rglob("*.rs"))
        for directory in VENDOR_DIRECTORIES:
            base = ROOT / "vendor" / directory
            expected.extend(base.rglob("*.rs"))
            expected.extend([base / "LICENSE", base / "UPSTREAM_Cargo.toml",
                             base / "UPSTREAM_VCS_INFO.json"])
        for path in expected:
            relative = path.relative_to(ROOT).as_posix()
            require(relative in archived, f"Missing bundled source or provenance: {relative}")
            require(read(relative) == path.read_bytes(), f"Packaged source differs: {relative}")
        require(b"CLT_WAL_PATCH_LEVEL" in read("vendor/turso_core/lib.rs"),
                "Core patch marker is missing")
        directories = {path.name for path in (ROOT / "vendor").iterdir() if path.is_dir()}
        require(directories == VENDOR_DIRECTORIES, "Unexpected unused vendored package")
        archived_directories = {Path(name).parts[1] for name in archived
                                if name.startswith("vendor/") and len(Path(name).parts) > 2}
        require(archived_directories == VENDOR_DIRECTORIES,
                "Archive contains an unexpected vendored package")
    print("Verified one CLT archive: bundled patched engine, pinned public dependencies, and tested source bytes.", flush=True)


def smoke_packaged_binary(target_dir):
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    binary = target_dir / "debug" / "clt"
    depfile = binary.with_suffix(".d")
    require(
        f"/package/clt-rs-{version}/src/main.rs" in depfile.read_text(),
        "Smoke check requires the native binary built from the CLT archive",
    )
    with tempfile.TemporaryDirectory(prefix="clt-release-smoke-") as temporary:
        root = Path(temporary)
        project = root / "project"
        project.mkdir()
        environment = {key: value for key, value in os.environ.items() if not key.startswith("CLT_")}
        state_file = root / "not-a-directory"
        state_file.touch()
        environment["CLT_AGENT_STATE_DIR"] = str(state_file / "state")

        def run(*arguments):
            result = subprocess.run(
                [str(binary), "--local", *arguments], cwd=project, env=environment,
                text=True, capture_output=True, check=True,
            )
            require(not result.stderr, f"Packaged binary wrote unexpected stderr: {result.stderr}")
            return result.stdout

        for flag in ("--version", "-V"):
            require(run(flag) == f"clt {version}\n", f"Incorrect packaged {flag} output")
        environment["CLT_AGENT_STATE_DIR"] = str(root / "state")
        run("init")
        run("add", "Release archive smoke check")
        require("Release archive smoke check" in run("list", "todo"), "Packaged task commands failed")
        run("agent", "register", ".")
        require("registered_projects=1" in run("agent", "status"), "Packaged registry did not persist registration")
        run("agent", "pause", ".")
        require("registered_projects=1 enabled=0" in run("agent", "status"), "Packaged registry did not persist pause")
        require((root / "state" / "registry.json").exists(), "Packaged registry did not create its recovery snapshot")
        require(not (root / "state" / "recovery-required").exists(), "Packaged registry requires recovery after smoke check")
    print("Packaged binary passed version, task, and registry persistence smoke checks.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-dirty", action="store_true", help="Allow local release preparation before committing")
    parser.add_argument("--audit-only", action="store_true", help="Audit the archive from an earlier packaging run")
    args = parser.parse_args()
    common = ["--locked"]
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", *common], cwd=ROOT
    ))
    if not args.audit_only:
        command = ["cargo", "package", *common]
        if args.allow_dirty:
            command.append("--allow-dirty")
        # Verification builds the extracted package without any workspace paths.
        subprocess.run(command, cwd=ROOT, check=True)
    audit_archives(Path(metadata["target_directory"]) / "package")
    if not args.audit_only:
        smoke_packaged_binary(Path(metadata["target_directory"]))


if __name__ == "__main__":
    main()
