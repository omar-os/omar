#!/usr/bin/env python3
"""check-repo-rules.sh against a scratch repository: documentation and binary
files a change adds fail; allowed files, edits and code pass."""
import subprocess
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / ".github" / "scripts" / "check-repo-rules.sh"


def main():
    with tempfile.TemporaryDirectory(prefix="omar-repo-rules-") as tmp:
        repo = Path(tmp)

        def git(*args):
            subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *args],
                           cwd=repo, check=True, capture_output=True)

        git("init", "-q", "-b", "main")
        (repo / "README.md").write_text("readme\n")
        (repo / "lang").mkdir()
        (repo / "lang" / "README.md").write_text("old doc\n")
        (repo / "logo.png").write_bytes(b"\x89PNG\r\n\x1a\n\0\0\0")
        git("add", "-A")
        git("commit", "-q", "-m", "base")
        git("tag", "base")

        def check(files):
            git("checkout", "-q", "-B", "change", "base")
            for name, content in files.items():
                path = repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                (path.write_bytes if isinstance(content, bytes) else path.write_text)(content)
            git("add", "-A")
            git("commit", "-q", "--allow-empty", "-m", "change")
            return subprocess.run([str(SCRIPT), "base"], cwd=repo, capture_output=True, text=True)

        allowed = {
            "src/main.rs": "fn main() {}\n",
            "README.md": "readme, edited\n",
            "lang/README.md": "existing doc, edited\n",
            "lang/spec.md": "spec\n",
            "prompts/skills/new.md": "prompt\n",
            "CONTRIBUTING.md": "rules\n",
            "AGENT.md": "agents\n",
            "CLAUDE.md": "@AGENT.md\n",
        }
        result = check(allowed)
        assert result.returncode == 0, result

        refused = {
            "docs/guide.md": "doc",
            "docs/diagram.txt": "doc",
            "web/NOTES.md": "doc",
            "src/docs/api.rst": "doc",
            "img/shot.png": b"\x89PNG\r\n\x1a\n\0\0\0",
            "logo.png": b"\x89PNG\r\n\x1a\n\0\0\1",
        }
        for name, content in refused.items():
            result = check({name: content})
            assert result.returncode == 1 and f"file={name}::" in result.stdout, (name, result)
    print("PASS: repository rules refuse added docs and binaries, allow code, edits and the exceptions")


if __name__ == "__main__":
    main()
