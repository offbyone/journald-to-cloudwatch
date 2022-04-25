import shutil
from pathlib import Path
import sys
from textwrap import dedent

import tomli
from invoke import call, task, Collection
from invoke.exceptions import UnexpectedExit


def read_version(cargo_toml):
    with cargo_toml.open(mode="rb") as rfile:
        cargo = tomli.load(rfile)
        return cargo["package"]["version"]


REPO_DIR = Path(__file__).parent.resolve()
SCRIPT_DIR = REPO_DIR / "tools"
CARGO_TOML = REPO_DIR / "Cargo.toml"
CARGO_LOCK = REPO_DIR / "Cargo.lock"
IMAGE_NAME = "build-jtc-image"
SRC_DIR = REPO_DIR / "src"
OUTPUT_DIR = REPO_DIR / "target"
DIST_DIR = REPO_DIR / "dist"
TAR_PATH = DIST_DIR / f"journald-to-cloudwatch-{read_version(CARGO_TOML)}.tar.gz"


def check_for_image(c, base_os):
    out = c.run(f"docker images -q {IMAGE_NAME}:{base_os}", hide="both")
    if out.stdout.strip():
        return True
    return False


def build_image_cmd(base_os):
    dockerfile = SCRIPT_DIR / f"Dockerfile.{base_os}"

    return rf"""docker buildx build \
    --load \
    -t {IMAGE_NAME}:{base_os} \
    -f {dockerfile} \
    .
    """


@task
def clean(c, clean_cargo=False):
    c.run("docker volume rm jtc-cache-volume")
    if clean_cargo:
        c.run("docker volume rm jtc-cargo-volume")


def build_project_cmd(base_os, release=True):
    build_flag = "--release" if release else ""
    return rf"""docker run --rm \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/cache \
    -v jtc-cargo-volume:/cargo \
    -v jtc-cache-volume:/cache \
    -v {OUTPUT_DIR}:/host:z \
    -v {SRC_DIR}:/build/src/ \
    -v {SCRIPT_DIR}:/build/tools/ \
    -v {CARGO_TOML}:/build/Cargo.toml \
    -v {CARGO_LOCK}:/build/Cargo.lock:rw \
    {IMAGE_NAME}:{base_os} build {build_flag}
    """


def tar_cmd():
    return rf"""tar czf \
    {TAR_PATH} \
    -C {DIST_DIR} \
    journald-to-cloudwatch \
    journald-to-cloudwatch.service"""


@task
def build_ubuntu_image(c, force=False):
    _build_image(c, "ubuntu", force)


@task
def build_amazonlinux2_image(c, force=False):
    _build_image(c, "amazonlinux2", force)


def _build_image(c, base_os, force):
    if force or not check_for_image(c, base_os):
        c.run(build_image_cmd(base_os))
    else:
        print(f"Skipping image build for {base_os}")


@task(pre=[build_ubuntu_image])
def build_ubuntu(c, release=False):
    _build(c, "ubuntu", release)


@task(pre=[build_amazonlinux2_image])
def build_amazonlinux2(c, release=False):
    _build(c, "amazonlinux2", release)


def _build(c, base_os, release):
    cmd = build_project_cmd(base_os, release)
    c.run(cmd)


@task(pre=[call(build_ubuntu, release=True)])
def publish_ubuntu(c):
    _publish(c)


@task(pre=[call(build_amazonlinux2, release=True)])
def publish_amazonlinux2(c):
    _publish(c)


def _publish(c):
    if TAR_PATH.exists():
        TAR_PATH.unlink()
    # copy the release build into place
    DIST_DIR.mkdir(exist_ok=True)

    shutil.copy2(
        OUTPUT_DIR / "release" / "journald-to-cloudwatch",
        dst=DIST_DIR / "journald-to-cloudwatch",
    )
    shutil.copy2(
        SCRIPT_DIR / "journald-to-cloudwatch.service",
        dst=DIST_DIR / "journald-to-cloudwatch.service",
    )
    c.run(tar_cmd())
