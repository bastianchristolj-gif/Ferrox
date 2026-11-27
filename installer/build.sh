#!/usr/bin/env bash
# Construit Ferrox pour Windows puis l'installateur Ferrox-Setup-<version>.exe (depuis Linux).
# Prérequis : rustup target add x86_64-pc-windows-gnu ; apt install mingw-w64 nsis zip
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
cargo build --release --target x86_64-pc-windows-gnu
rm -rf dist && mkdir -p dist/Ferrox
cp target/x86_64-pc-windows-gnu/release/ferrox.exe dist/Ferrox/Ferrox.exe
cp target/x86_64-pc-windows-gnu/release/build/webview2-com-sys-*/out/x64/WebView2Loader.dll dist/Ferrox/
cp installer/LISEZMOI.txt dist/Ferrox/
(cd dist && zip -qr "Ferrox-${VERSION}-windows-x64.zip" Ferrox)
(cd installer && makensis -V2 -DVERSION="$VERSION" ferrox.nsi)
ls -lh dist/*.exe dist/*.zip
