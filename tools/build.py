#!/usr/bin/env python3
from pathlib import Path
import os
import subprocess
import sys

try:
    import tomli
except ModuleNotFoundError:
    print('missing toml package; try "pip3 install --user toml"')
    sys.exit(1)

SCRIPT_DIR = Path(__file__).parent.resolve()
REPO_DIR = (SCRIPT_DIR / "..").resolve()
src = REPO_DIR / "src"
tools = REPO_DIR / "tools"
cargo_toml = REPO_DIR / "Cargo.toml"
cargo_lock = REPO_DIR / "Cargo.lock"


def run_cmd(*cmd):
    print(" ".join(cmd))
    subprocess.check_call(cmd)


def read_version():
    with cargo_toml.open(mode="rb") as rfile:
        cargo = tomli.load(rfile)
        return cargo["package"]["version"]


def main():
    version = read_version()
    image_name = "jtc-image"
    output_dir = os.path.join(REPO_DIR, "release")
    exe_path = os.path.join(output_dir, "journald-to-cloudwatch")
    tar_path = os.path.join(
        output_dir, "journald-to-cloudwatch-{}.tar.gz".format(version)
    )

    run_cmd(
        "docker",
        "run",
        "-e",
        "CARGO_HOME=/cargo",
        "-e",
        "CARGO_TARGET_DIR=/cache",
        "-v",
        "jtc-cargo-volume:/cargo",
        "-v",
        "jtc-cache-volume:/cache",
        "-v",
        "{}:/host:z".format(output_dir),
        "-v",
        f"{src}:/build/src/",
        "-v",
        f"{tools}:/build/tools/",
        "-v",
        f"{cargo_toml}:/build/Cargo.toml",
        "-v",
        f"{cargo_lock}:/build/Cargo.lock:rw",
        image_name,
    )
    run_cmd("chown", "{}:{}".format(os.getuid(), os.getgid()), exe_path)
    if os.path.exists(tar_path):
        os.remove(tar_path)
    run_cmd(
        "tar",
        "czf",
        tar_path,
        "-C",
        output_dir,
        os.path.basename(exe_path),
        "journald-to-cloudwatch.service",
    )


if __name__ == "__main__":
    main()
