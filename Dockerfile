# Groow's body. Disposable: rebuilt from this file. Everything Groow is lives in /home/groow.
#
# Two inhabitants share it. The core runs as root: it owns the state, decides what happens
# next, and starts everything else. The mind runs as an ordinary user with no write access to
# any of that. The brain, which needs the card, is Python and also runs as root because the
# weights are part of the state.
#
# Volta (V100) needs CUDA 12.x images and the cu126 torch wheels.

# ---------------------------------------------------------------- the core, built once
FROM rust:1.90-slim-bookworm AS core
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
# The schema, which the build turns into the wire types. protoc comes with the build itself,
# so nothing needs installing for it.
COPY nervous_system/proto /nervous_system/proto
COPY rust/Cargo.toml rust/Cargo.lock* ./
COPY rust/crates ./crates
RUN cargo build --release --locked 2>/dev/null || cargo build --release

# ---------------------------------------------------------------- the body
FROM nvidia/cuda:12.6.3-cudnn-runtime-ubuntu24.04

ENV DEBIAN_FRONTEND=noninteractive PYTHONUNBUFFERED=1 UV_LINK_MODE=copy \
    PATH=/opt/venv/bin:/usr/local/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3.12 python3.12-venv curl ca-certificates xz-utils git bash sqlite3 \
    && rm -rf /var/lib/apt/lists/* \
    && curl -LsSf https://astral.sh/uv/install.sh | sh \
    && install -m 0755 /root/.local/bin/uv /usr/local/bin/uv

# the learning side: torch, the model, the judge
RUN uv venv --python 3.12 /opt/venv \
    && uv pip install --python /opt/venv/bin/python --index-url https://download.pytorch.org/whl/cu126 torch
COPY pyproject.toml README.md /opt/groow/
COPY neuro /opt/groow/neuro
RUN uv pip install --python /opt/venv/bin/python /opt/groow sentencepiece protobuf "huggingface_hub[hf_xet]"

# the core, and the commands the mind can run
COPY --from=core /src/target/release/groow /usr/local/bin/groow
COPY skel /usr/share/groow/skel
RUN chmod 0755 /usr/local/bin/groow && chmod -R a+rX,go-w /usr/share/groow/skel

# the mind: an ordinary user who owns nothing but its own home corner
RUN (getent passwd 1000 && userdel -r "$(getent passwd 1000 | cut -d: -f1)" || true) \
    && useradd -m -u 1000 -s /bin/bash groow \
    && mkdir -p /nix && chown groow:groow /nix
COPY docker/entrypoint.sh /usr/local/bin/groow-entrypoint
RUN chmod 0755 /usr/local/bin/groow-entrypoint

WORKDIR /home/groow
ENV HOME=/home/groow GROOW_BODY=sandbox HF_HOME=/home/groow/.cache/huggingface \
    GROOW_SKEL=/usr/share/groow/skel
VOLUME ["/home/groow", "/nix"]
ENTRYPOINT ["/usr/local/bin/groow-entrypoint"]
CMD ["start", "--as-user", "groow"]
