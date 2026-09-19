#!/usr/bin/env bash
# Wake Groow in the container this project ships with, and wait until it can actually answer.
#
# This is the runtime, not the creature. `groow start` runs the core and knows nothing about
# containers; deciding that the core should run inside this one is this script's job, and would
# be a different script for a microvm or a machine of its own.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! docker --version >/dev/null 2>&1; then
  echo "docker is not installed, so there is no body to wake. \`groow start\` runs the core here." >&2
  exit 1
fi
if ! docker image inspect groow:latest >/dev/null 2>&1; then
  echo "no body yet; building it (a few minutes, once)…" >&2
  docker compose build groow
fi
[ -f home/state/birth.json ] || \
  echo "no birth certificate yet, so this is a birth: it will fetch its base model, about 8 GB, once." >&2

# Always let compose decide. It leaves a container alone when nothing has changed and replaces
# it when the image has, which is what should happen after a rebuild.
docker compose up -d groow

# Being up is not being awake: the core answers in a moment, the weights take the best part of
# a minute, and until they are loaded there is nothing a turn could do.
echo "waiting for it to wake (it has to load its weights first)…" >&2
for _ in $(seq 900); do
  if docker compose exec -T -u 0 groow groow --home /home/groow status --json 2>/dev/null \
     | tr -d ' \n' | grep -q '"brain":true'; then
    docker compose exec -T -u 0 groow groow --home /home/groow status
    echo >&2
    echo "\`groow ui\` opens the window. \`make stop\` puts its body back to sleep." >&2
    exit 0
  fi
  sleep 2
done
echo "it did not wake within thirty minutes; \`make logs\` shows what its body is doing" >&2
exit 1
