#!/usr/bin/env python3
"""Keep the Homebrew formula and npm installer pinned to one release."""

import argparse
import json
import pathlib
import re
import sys

TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
]

ROOT = pathlib.Path(__file__).resolve().parents[1]
FORMULA = ROOT / "Formula" / "whisker.rb"
CHECKSUMS_JSON = ROOT / "npm" / "whisker" / "checksums.json"
PACKAGE_JSON = ROOT / "npm" / "whisker" / "package.json"

REPO = "bugeng777/whisker"


def parse_sums(path: pathlib.Path) -> dict[str, str]:
    found: dict[str, str] = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        digest, name = line.split(None, 1)
        name = name.lstrip("*")
        prefix = "whisker-"
        suffix = ".tar.gz"
        if not name.startswith(prefix) or not name.endswith(suffix):
            continue
        found[name[len(prefix) : -len(suffix)]] = digest.lower()
    missing = [target for target in TARGETS if target not in found]
    if missing:
        raise SystemExit(f"checksums missing: {', '.join(missing)}")
    return {target: found[target] for target in TARGETS}


def version_of(tag: str) -> str:
    version = tag.strip().lstrip("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise SystemExit(f"expected a vX.Y.Z tag, got {tag}")
    return version


def formula_text(version: str, sums: dict[str, str]) -> str:
    def block(target: str) -> str:
        url = (
            f"https://github.com/{REPO}/releases/download/"
            f"v{version}/whisker-{target}.tar.gz"
        )
        return f'      url "{url}"\n      sha256 "{sums[target]}"'

    return f"""class Whisker < Formula
  desc "Terminal coding agent for OpenAI-compatible models"
  homepage "https://github.com/{REPO}"
  version "{version}"
  license "MIT"

  on_macos do
    on_arm do
{block("aarch64-apple-darwin")}
    end
    on_intel do
{block("x86_64-apple-darwin")}
    end
  end

  on_linux do
    on_arm do
{block("aarch64-unknown-linux-gnu")}
    end
    on_intel do
{block("x86_64-unknown-linux-gnu")}
    end
  end

  def install
    bin.install "whisker"
    if File.directory?("skills/impeccable")
      (pkgshare/"skills").install "skills/impeccable"
    end
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/whisker --version")
  end
end
"""


def update_package_version(version: str) -> None:
    text = PACKAGE_JSON.read_text()
    updated, count = re.subn(
        r'"version"\s*:\s*"[^"]+"',
        f'"version": "{version}"',
        text,
        count=1,
    )
    if count != 1:
        raise SystemExit("package.json is missing a version")
    PACKAGE_JSON.write_text(updated)


def write_all(version: str, sums: dict[str, str]) -> None:
    FORMULA.parent.mkdir(parents=True, exist_ok=True)
    FORMULA.write_text(formula_text(version, sums))
    CHECKSUMS_JSON.parent.mkdir(parents=True, exist_ok=True)
    payload = {"version": version, "targets": sums}
    CHECKSUMS_JSON.write_text(json.dumps(payload, indent=2) + "\n")
    update_package_version(version)


def parse_formula(text: str) -> tuple[str, dict[str, str]]:
    version_match = re.search(r'^  version "([^"]+)"$', text, re.M)
    if version_match is None:
        raise SystemExit("formula is missing version")
    pattern = re.compile(
        r'url "https://github.com/bugeng777/whisker/releases/download/'
        r'v[^"/]+/whisker-([^"]+)\.tar\.gz"\n\s+sha256 "([0-9a-f]{64})"'
    )
    found = {target: digest for target, digest in pattern.findall(text)}
    missing = [target for target in TARGETS if target not in found]
    if missing:
        raise SystemExit(f"formula missing checksums: {', '.join(missing)}")
    return version_match.group(1), {target: found[target] for target in TARGETS}


def check_committed() -> None:
    formula_version, formula_sums = parse_formula(FORMULA.read_text())
    checksums = json.loads(CHECKSUMS_JSON.read_text())
    package = json.loads(PACKAGE_JSON.read_text())
    errors: list[str] = []
    if checksums.get("version") != formula_version:
        errors.append("checksums.json version does not match Formula/whisker.rb")
    if package.get("version") != formula_version:
        errors.append("package.json version does not match Formula/whisker.rb")
    if checksums.get("targets") != formula_sums:
        errors.append("checksums.json targets do not match Formula/whisker.rb")
    expected = formula_text(formula_version, formula_sums)
    if FORMULA.read_text() != expected:
        errors.append("Formula/whisker.rb drifted from the generator")
    if errors:
        raise SystemExit("\n".join(errors))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", help="release tag, such as v0.6.1")
    parser.add_argument("--checksums", type=pathlib.Path, help="SHA256SUMS file")
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the formula, checksums, and package version agree",
    )
    args = parser.parse_args()
    if args.check:
        check_committed()
        return
    if not args.version or args.checksums is None:
        parser.error("--version and --checksums are required unless --check")
    write_all(version_of(args.version), parse_sums(args.checksums))


if __name__ == "__main__":
    main()
