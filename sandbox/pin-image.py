#!/usr/bin/env python3
"""Give a docker-save OCI archive a digest reference for local sbx import.
No image blobs change; only the archive's reference metadata is rewritten.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    if args.source.resolve() == args.destination.resolve():
        parser.error("source and destination must differ")
    with tarfile.open(args.source) as source:
        index = json.load(source.extractfile("index.json"))
        if len(index["manifests"]) != 1:
            parser.error("save exactly one image in the archive")
        descriptor = index["manifests"][0]
        digest = descriptor["digest"]
        algorithm, checksum = digest.split(":", 1)
        if algorithm != "sha256":
            parser.error("expected a SHA-256 image digest")
        blob = source.extractfile("blobs/sha256/" + checksum).read()
        if hashlib.sha256(blob).hexdigest() != checksum:
            parser.error("image manifest digest mismatch")
        repository = descriptor["annotations"]["io.containerd.image.name"].split("@")[0]
        if ":" in repository.rsplit("/", 1)[-1]:
            repository = repository.rsplit(":", 1)[0]
        reference = repository + "@" + digest
        descriptor["annotations"] = {"io.containerd.image.name": reference}
        payload = json.dumps(index).encode()
        # Exclusive output avoids replacing an existing image archive.
        with tarfile.open(args.destination, "x") as target:
            for member in source:
                # Prefer OCI reference metadata over Docker's mutable RepoTags.
                if member.name == "manifest.json":
                    continue
                data = source.extractfile(member) if member.isfile() else None
                if member.name == "index.json":
                    member.size = len(payload)
                    data = io.BytesIO(payload)
                target.addfile(member, data)
    print(reference)


if __name__ == "__main__":
    main()
