#!/usr/bin/env python3
"""Build the one-job adapter over an explicitly pinned existing prover image."""

import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

import job
from runpod import require


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-image", required=True)
    parser.add_argument("--release", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--check", action="store_true", help="Docker static check only; does not qualify a proving image")
    args = parser.parse_args()
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9./:_-]*@sha256:[0-9a-f]{64}", args.base_image),
            "base_image_requires_digest")
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9./:_-]*", args.tag), "invalid_image_tag")
    release = job.read_file(args.release, job.MAX_MANIFEST)
    job.release_identity(release)
    if not args.execute:
        print("dry-run: pinned image and release metadata validated; no Docker action")
        return
    source = Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory(prefix="zksys-rental-image-") as temporary:
        context = Path(temporary)
        for name in ("Dockerfile", "runpod.py", "job.py", "worker.py"):
            shutil.copyfile(source / name, context / name)
        job.write_new(context / "release.json", release)
        command = ["docker", "build", "--platform", "linux/amd64", "--build-arg", "PROVER_BASE_IMAGE=" + args.base_image,
                   "--tag", args.tag]
        if args.check:
            command.append("--check")
        subprocess.run([*command, str(context)], check=True)


if __name__ == "__main__":
    main()
