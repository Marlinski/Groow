#!/usr/bin/env bash
# Groow's body wakes up: prepare the home, install Nix on first start, give birth if there is no
# birth certificate yet, then run the command. This file is part of the body: Groow cannot change it.
# Runs as the unprivileged user `groow`; the only writable place is $HOME (a volume).
set -e
export HOME=/home/groow
cd "$HOME"
mkdir -p "$HOME/.cache/tmp" "$HOME/state" "$HOME/workspace" "$HOME/.config/nix"
export TMPDIR="$HOME/.cache/tmp"          # /tmp may be noexec; installers and builds need an executable scratch dir
[ -f "$HOME/.config/nix/nix.conf" ] || echo "experimental-features = nix-command flakes" > "$HOME/.config/nix/nix.conf"

# Nix, single-user. /nix is a mount backed by ./home/.nix on the host (a symlinked store is refused by Nix).
if [ ! -x "$HOME/.nix-profile/bin/nix" ] && [ ! -x "$HOME/.local/state/nix/profiles/profile/bin/nix" ]; then
  if [ -z "$GROOW_SKIP_NIX" ]; then
    echo "groow-body: installing nix into the home (first start)…"
    curl -fsSL https://nixos.org/nix/install -o "$TMPDIR/nix-install.sh"
    sh "$TMPDIR/nix-install.sh" --no-daemon --yes >"$HOME/.cache/nix-install.log" 2>&1 \
      && echo "groow-body: nix installed" || { echo "groow-body: nix install failed"; tail -20 "$HOME/.cache/nix-install.log"; }
  fi
fi
for f in "$HOME/.nix-profile/etc/profile.d/nix.sh"; do [ -f "$f" ] && . "$f"; done
export PATH="$HOME/.venv/bin:$HOME/.local/bin:$HOME/.nix-profile/bin:/opt/venv/bin:$PATH"
[ -f "$HOME/groow.json" ] || cp /opt/groow/groow.json "$HOME/groow.json" 2>/dev/null || true
grep -q "nix.sh" "$HOME/.bashrc" 2>/dev/null || cat >> "$HOME/.bashrc" <<'RC'
# groow home
[ -f "$HOME/.nix-profile/etc/profile.d/nix.sh" ] && . "$HOME/.nix-profile/etc/profile.d/nix.sh"
export PATH="$HOME/.venv/bin:$HOME/.local/bin:$HOME/.nix-profile/bin:/opt/venv/bin:$PATH"
export TMPDIR="$HOME/.cache/tmp"
RC
# Birth or waking: no birth certificate in the home means this is the first time.
if [ "${1:-}" = "start" ] && [ ! -f "$HOME/state/birth.json" ]; then
  echo "groow-body: no birth certificate in the home. This is a birth: fetching the base model into the home…"
  groow init
  rm -rf "$HF_HOME/hub/models--"* 2>/dev/null || true     # the working copy is state/base; the download cache would double the home
fi
exec "$@"
