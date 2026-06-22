#!/usr/bin/env python3

import argparse
import hashlib
import sys
import urllib.request

REPO = "baicai-1145/toksqz"
BIN = "toksqz"
TARGETS = {
    "darwin_arm": "aarch64-apple-darwin",
    "darwin_intel": "x86_64-apple-darwin",
    "linux_arm": "aarch64-unknown-linux-musl",
    "linux_intel": "x86_64-unknown-linux-musl",
}


def asset_url(version: str, target: str) -> str:
    asset = f"{BIN}-v{version}-{target}.tar.gz"
    return f"https://github.com/{REPO}/releases/download/v{version}/{asset}"


def sha256_for(url: str) -> str:
    with urllib.request.urlopen(url) as response:
        data = response.read()
    return hashlib.sha256(data).hexdigest()


def render(version: str, shas: dict[str, str]) -> str:
    return f"""class Toksqz < Formula
  desc "Lightweight Rust proxy for LLM prompt compression"
  homepage "https://github.com/{REPO}"
  version "{version}"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "{asset_url(version, TARGETS['darwin_arm'])}"
      sha256 "{shas['darwin_arm']}"
    end

    on_intel do
      url "{asset_url(version, TARGETS['darwin_intel'])}"
      sha256 "{shas['darwin_intel']}"
    end
  end

  on_linux do
    on_arm do
      url "{asset_url(version, TARGETS['linux_arm'])}"
      sha256 "{shas['linux_arm']}"
    end

    on_intel do
      url "{asset_url(version, TARGETS['linux_intel'])}"
      sha256 "{shas['linux_intel']}"
    end
  end

  def install
    bin.install "{BIN}"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/{BIN} --version")
  end
end
"""


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate Homebrew formula for toksqz.")
    parser.add_argument("version", help="Release version without leading v, e.g. 0.1.2")
    parser.add_argument("-o", "--output", help="Write formula to file instead of stdout")
    args = parser.parse_args()

    shas = {name: sha256_for(asset_url(args.version, target)) for name, target in TARGETS.items()}
    formula = render(args.version, shas)

    if args.output:
        with open(args.output, "w", encoding="utf-8") as handle:
            handle.write(formula)
    else:
        sys.stdout.write(formula)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
