#!/usr/bin/env bash
# The body wakes up: prepare the home, start the brain, then hand over to the core.
# This file is part of the body. The mind cannot change it, and neither can the core.
set -e
export HOME=/home/groow
cd "$HOME"

# The state is the core's and it makes it itself; everything else here is the mind's, and the
# core fills it in on a first start: skills, commands, the manual, somewhere to work.
STATE="$HOME/state"
mkdir -p "$STATE" "$HOME/.cache/tmp" "$HOME/.config/nix"
export TMPDIR="$HOME/.cache/tmp"     # /tmp may be mounted without exec; installers need somewhere to run
chown -R groow:groow "$HOME/.cache" "$HOME/.config" 2>/dev/null || true
[ -f "$HOME/groow.json" ] || cp /opt/groow/groow.json "$HOME/groow.json"

# Nix, installed into the home as the mind, so it can add tools for itself without root.
if [ -z "$GROOW_SKIP_NIX" ] && [ ! -x "$HOME/.nix-profile/bin/nix" ]; then
  echo "body: installing nix into the home (first start, about thirty seconds)…"
  [ -f "$HOME/.config/nix/nix.conf" ] || echo "experimental-features = nix-command flakes" > "$HOME/.config/nix/nix.conf"
  curl -fsSL https://nixos.org/nix/install -o "$TMPDIR/nix-install.sh"
  runuser -u groow -- sh "$TMPDIR/nix-install.sh" --no-daemon --yes > "$HOME/.cache/nix-install.log" 2>&1 \
    && echo "body: nix installed" || { echo "body: nix install failed"; tail -5 "$HOME/.cache/nix-install.log"; }
fi
grep -q "groow home" "$HOME/.bashrc" 2>/dev/null || cat >> "$HOME/.bashrc" <<'RC'
# groow home
[ -f "$HOME/.nix-profile/etc/profile.d/nix.sh" ] && . "$HOME/.nix-profile/etc/profile.d/nix.sh"
export PATH="$HOME/bin:$HOME/.nix-profile/bin:/usr/local/bin:/usr/bin:/bin"
export TMPDIR="$HOME/.cache/tmp"
RC

# The brain: it holds the card and the weights, so it starts first and keeps running.
echo "body: starting the brain"
/opt/venv/bin/python -m neuro.serve --config "$HOME/groow.json" --state "$STATE" --port "${GROOW_BRAIN_PORT:-7374}" &
BRAIN=$!
trap 'kill $BRAIN 2>/dev/null || true' EXIT

echo "body: waking groow ($*)"
exec /usr/local/bin/groow --config "$HOME/groow.json" --state "$STATE" "$@"
