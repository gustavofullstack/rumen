#!/bin/sh
# Instala o RUMEN só para o usuário atual (sem sudo).
set -e
cd "$(dirname "$0")/.."
install -Dm755 target/release/rumen "$HOME/.local/bin/rumen"
install -Dm644 packaging/rumen.desktop "$HOME/.local/share/applications/rumen.desktop"
install -Dm644 packaging/rumen.svg "$HOME/.local/share/icons/hicolor/scalable/apps/rumen.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "RUMEN instalado em ~/.local/bin/rumen"
