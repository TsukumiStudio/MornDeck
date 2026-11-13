#!/usr/bin/env python3
"""winget のマニフェスト (version / defaultLocale / installer の 3 つ) を作る。
使い方: winget.py <版> <出力フォルダ> <x64 の zip> <arm64 の zip>
zip の SHA256 を計算し、GitHub Releases の URL で書く。出力は <出力フォルダ>/manifests/t/TsukumiStudio/MornDeck/<版>/"""
import hashlib, os, sys

ID = "TsukumiStudio.MornDeck"
PUBLISHER = "TSUKUMI STUDIO"
REPO = "https://github.com/TsukumiStudio/MornDeck"
MANIFEST_VERSION = "1.12.0"
# WSL 2 と wsl --install が使える Windows 10 2004
MIN_OS = "10.0.19041.0"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest().upper()


def main():
    version, out, x64, arm64 = sys.argv[1:5]
    d = os.path.join(out, "manifests", "t", "TsukumiStudio", "MornDeck", version)
    os.makedirs(d, exist_ok=True)
    url = lambda z: f"{REPO}/releases/download/v{version}/{os.path.basename(z)}"
    files = {
        f"{ID}.yaml": f"""PackageIdentifier: {ID}
PackageVersion: {version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: {MANIFEST_VERSION}
""",
        f"{ID}.locale.en-US.yaml": f"""PackageIdentifier: {ID}
PackageVersion: {version}
PackageLocale: en-US
Publisher: {PUBLISHER}
PublisherUrl: https://github.com/TsukumiStudio
PackageName: MornDeck
PackageUrl: {REPO}
License: Unlicense
LicenseUrl: {REPO}/blob/main/LICENSE
ShortDescription: Terminal multiplexer for AI coding agents, running in WSL
Description: MornDeck runs Claude Code, Codex CLI and GitHub Copilot CLI in tabs inside WSL, with a sidebar that shows whether each agent is working, waiting or done. Requires WSL 2 with a Linux distribution.
Moniker: mdeck
Tags:
- ai
- claude-code
- codex
- terminal
- tui
- wsl
ReleaseNotesUrl: {REPO}/releases/tag/v{version}
ManifestType: defaultLocale
ManifestVersion: {MANIFEST_VERSION}
""",
        f"{ID}.installer.yaml": f"""PackageIdentifier: {ID}
PackageVersion: {version}
MinimumOSVersion: {MIN_OS}
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: mdeck.exe
  PortableCommandAlias: mdeck
Installers:
- Architecture: x64
  InstallerUrl: {url(x64)}
  InstallerSha256: {sha256(x64)}
- Architecture: arm64
  InstallerUrl: {url(arm64)}
  InstallerSha256: {sha256(arm64)}
ManifestType: installer
ManifestVersion: {MANIFEST_VERSION}
""",
    }
    for name, text in files.items():
        kind = text.split("ManifestType: ")[1].split()[0]
        header = f"# yaml-language-server: $schema=https://aka.ms/winget-manifest.{kind}.{MANIFEST_VERSION}.schema.json\n\n"
        with open(os.path.join(d, name), "w", encoding="utf-8", newline="\n") as f:
            f.write(header + text)
    print(d)


main()
