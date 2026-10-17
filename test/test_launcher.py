import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from support import REPO  # noqa: E402

FAKE_BINARY = '#!/bin/sh\necho "ran $0 $*"\n'

FAKE_CURL = """#!/bin/sh
while [ $# -gt 1 ]; do
  case "$1" in -o) out=$2; shift ;; esac
  shift
done
echo "$1" >> "$CURL_LOG"
cp "$CURL_SERVES" "$out"
"""


class Launcher(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.root = self.tmp / "plugin"
        (self.root / "bin").mkdir(parents=True)
        shutil.copy(REPO / "bin" / "nexthop", self.root / "bin" / "nexthop")
        (self.root / "manifest.json").write_text(json.dumps({"version": "9.9.9"}))
        self.served = self.tmp / "served"
        self.served.write_text(FAKE_BINARY)
        self.pin(FAKE_BINARY)
        self.origin("git@github.com:someone/omarchy-nexthop.git")
        stubs = self.tmp / "stubs"
        stubs.mkdir()
        (stubs / "curl").write_text(FAKE_CURL)
        (stubs / "curl").chmod(0o755)
        self.log = self.tmp / "curl.log"
        self.env = dict(os.environ, PATH=f"{stubs}:{os.environ['PATH']}",
                        CURL_LOG=str(self.log), CURL_SERVES=str(self.served))

    def pin(self, content):
        digest = hashlib.sha256(content.encode()).hexdigest()
        (self.root / "bin" / "nexthop.sha256").write_text(f"{digest}  nexthop-x86_64\n")

    def origin(self, url):
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(["git", "-C", str(self.root), "remote", "remove", "origin"],
                       capture_output=True)
        subprocess.run(["git", "-C", str(self.root), "remote", "add", "origin", url], check=True)

    def run_launcher(self, *args):
        return subprocess.run([str(self.root / "bin" / "nexthop"), *args], env=self.env,
                              capture_output=True, text=True, timeout=30)

    def downloads(self):
        return self.log.read_text().split() if self.log.exists() else []

    def test_fetches_the_release_of_its_own_origin_once_then_runs_it(self):
        first = self.run_launcher("stream", "live")
        self.assertEqual(first.returncode, 0, first.stderr)
        exe = self.root / "bin" / ".cache" / "9.9.9" / "nexthop"
        self.assertEqual(first.stdout, f"ran {exe} stream live\n")
        self.run_launcher("live")
        self.assertEqual(self.downloads(), [
            "https://github.com/someone/omarchy-nexthop/releases/download/v9.9.9/nexthop-x86_64"])

    def test_a_download_that_does_not_match_the_pinned_checksum_is_refused(self):
        self.served.write_text(FAKE_BINARY + "tampered\n")
        result = self.run_launcher("daemon")
        self.assertEqual(result.returncode, 1)
        self.assertIn("expects", result.stderr)
        self.assertEqual(list((self.root / "bin" / ".cache" / "9.9.9").iterdir()), [])

    def test_an_origin_that_is_not_github_is_refused_without_a_download(self):
        self.origin("https://gitlab.com/someone/omarchy-nexthop.git")
        result = self.run_launcher("daemon")
        self.assertEqual(result.returncode, 1)
        self.assertIn("is not a GitHub repository", result.stderr)
        self.assertEqual(self.downloads(), [])

    def test_an_update_replaces_the_previous_version(self):
        self.run_launcher("live")
        (self.root / "manifest.json").write_text(json.dumps({"version": "9.9.10"}))
        self.run_launcher("live")
        cached = sorted(p.name for p in (self.root / "bin" / ".cache").iterdir() if p.is_dir())
        self.assertEqual(cached, ["9.9.10"])


if __name__ == "__main__":
    unittest.main()
