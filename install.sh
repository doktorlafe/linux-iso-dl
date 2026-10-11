#!/bin/sh
# Sestaví aktuální verzi a nainstaluje ji do ~/.local/bin.
# Spouštět jako `sh install.sh` (na síťovém disku soubory nemívají právo spouštění).
set -eu

cd "$(dirname "$0")"
bin_dir="$HOME/.local/bin"
# Mimo repo: na takovém disku by build skripty závislostí nešly spustit.
export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/linux-iso-dl-target"

if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        PATH="$HOME/.cargo/bin:$PATH"
    else
        echo "chyba: chybí cargo, nainstalujte Rust z https://rustup.rs" >&2
        exit 1
    fi
fi

cargo build --release --locked
install -Dm755 "$CARGO_TARGET_DIR/release/linux-iso-dl" "$bin_dir/linux-iso-dl"
echo
echo "nainstalováno: $bin_dir/linux-iso-dl"

case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) echo "pozor: $bin_dir není v PATH" ;;
esac
