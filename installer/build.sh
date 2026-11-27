#!/usr/bin/env bash
# Construit Ferrox pour Windows, l'installateur, puis le zip de distribution :
#   dist/Ferrox-<version>-windows.zip
#     ├── Ferrox-Setup-<version>.exe   (installateur)
#     ├── Ferrox-portable/             (sans installation)
#     └── SHA256SUMS.txt               (empreintes pour vérifier l'intégrité)
# Prérequis (Linux) : rustup target add x86_64-pc-windows-gnu ; apt install mingw-w64 nsis zip
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
cargo build --release --target x86_64-pc-windows-gnu
rm -rf dist && mkdir -p dist/Ferrox
cp target/x86_64-pc-windows-gnu/release/ferrox.exe dist/Ferrox/Ferrox.exe
cp target/x86_64-pc-windows-gnu/release/build/webview2-com-sys-*/out/x64/WebView2Loader.dll dist/Ferrox/
cp installer/LISEZMOI.txt dist/Ferrox/
(cd installer && makensis -V2 -DVERSION="$VERSION" ferrox.nsi)

PKG="dist/Ferrox-${VERSION}-windows"
mkdir -p "$PKG/Ferrox-portable"
cp dist/Ferrox/* "$PKG/Ferrox-portable/"
cp "dist/Ferrox-Setup-${VERSION}.exe" "$PKG/"
(cd "$PKG" && sha256sum "Ferrox-Setup-${VERSION}.exe" Ferrox-portable/* > SHA256SUMS.txt)
(cd dist && zip -qr "Ferrox-${VERSION}-windows.zip" "Ferrox-${VERSION}-windows")
cat "$PKG/SHA256SUMS.txt"
ls -lh "dist/Ferrox-${VERSION}-windows.zip"
