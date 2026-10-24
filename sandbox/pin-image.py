#!/usr/bin/env python3
"""Give a docker-save OCI archive a digest reference for local sbx import.
Config and layer bytes are preserved; reference metadata is pinned.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile


def add_bytes(target, name, data):
    member = tarfile.TarInfo(name)
    member.size = len(data)
    target.addfile(member, io.BytesIO(data))


def canonical_name(name, digest):
    repository = name.split("@")[0]
    if ":" in repository.rsplit("/", 1)[-1]:
        repository = repository.rsplit(":", 1)[0]
    first = repository.split("/", 1)[0]
    if "/" not in repository:
        repository = "docker.io/library/" + repository
    elif "." not in first and ":" not in first and first != "localhost":
        repository = "docker.io/" + repository
    return repository + "@" + digest


def convert_legacy(source, destination, parser):
    images = json.load(source.extractfile("manifest.json"))
    if len(images) != 1 or not images[0].get("RepoTags"):
        parser.error("save exactly one named image in the archive")
    image = images[0]
    with tarfile.open(destination, "x") as target:
        def copy_blob(name, media_type):
            digest = hashlib.sha256()
            size = 0
            first = b""
            with source.extractfile(name) as data:
                while chunk := data.read(1024 * 1024):
                    if not first:
                        first = chunk[:4]
                    digest.update(chunk)
                    size += len(chunk)
            if media_type.endswith(".tar"):
                if first.startswith(b"\x1f\x8b"):
                    media_type += "+gzip"
                elif first == b"\x28\xb5\x2f\xfd":
                    media_type += "+zstd"
            checksum = digest.hexdigest()
            member = tarfile.TarInfo("blobs/sha256/" + checksum)
            member.size = size
            with source.extractfile(name) as data:
                target.addfile(member, data)
            return dict(mediaType=media_type, digest="sha256:" + checksum, size=size)

        config = copy_blob(image["Config"], "application/vnd.oci.image.config.v1+json")
        layers = [copy_blob(name, "application/vnd.oci.image.layer.v1.tar") for name in image["Layers"]]
        manifest = dict(schemaVersion=2, mediaType="application/vnd.oci.image.manifest.v1+json",
                        config=config, layers=layers)
        payload = json.dumps(manifest).encode()
        checksum = hashlib.sha256(payload).hexdigest()
        reference = canonical_name(image["RepoTags"][0], "sha256:" + checksum)
        add_bytes(target, "blobs/sha256/" + checksum, payload)
        descriptor = dict(mediaType=manifest["mediaType"], digest="sha256:" + checksum,
                          size=len(payload), annotations={"io.containerd.image.name": reference})
        add_bytes(target, "index.json", json.dumps(dict(schemaVersion=2, manifests=[descriptor])).encode())
        add_bytes(target, "oci-layout", b'{"imageLayoutVersion":"1.0.0"}')
    return reference


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    if args.source.resolve() == args.destination.resolve():
        parser.error("source and destination must differ")
    with tarfile.open(args.source) as source:
        if "index.json" not in source.getnames():
            print(convert_legacy(source, args.destination, parser))
            return
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
        reference = canonical_name(descriptor["annotations"]["io.containerd.image.name"], digest)
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
