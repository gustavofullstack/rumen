#!/bin/sh
# RUMEN — instalador de um comando para Linux x86_64 (sem sudo).
#   curl -fsSL https://gustavofullstack.github.io/rumen/install.sh | sh
set -eu
REPO="gustavofullstack/rumen"
ASSET="rumen-x86_64-linux.tar.gz"
URL="https://github.com/$REPO/releases/latest/download/$ASSET"
SUMS="https://github.com/$REPO/releases/latest/download/SHA256SUMS"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) ;;
  *) echo "RUMEN: por enquanto só há build para Linux x86_64." >&2; exit 1 ;;
esac

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
echo "→ Baixando RUMEN..."
curl -fL --proto '=https' --tlsv1.2 -o "$tmp/$ASSET" "$URL"
curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/SHA256SUMS" "$SUMS"
echo "→ Verificando integridade (SHA-256)..."
(cd "$tmp" && grep " $ASSET\$" SHA256SUMS | sha256sum -c -)
tar -xzf "$tmp/$ASSET" -C "$tmp"
install -Dm755 "$tmp/rumen/rumen" "$HOME/.local/bin/rumen"
install -Dm644 "$tmp/rumen/rumen.desktop" "$HOME/.local/share/applications/rumen.desktop"
sed -i "s#^Exec=rumen#Exec=$HOME/.local/bin/rumen#" "$HOME/.local/share/applications/rumen.desktop"
install -Dm644 "$tmp/rumen/rumen.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/rumen.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "✓ RUMEN instalado. Abra pelo menu ou rode: rumen"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "  (adicione ~/.local/bin ao PATH)";; esac
