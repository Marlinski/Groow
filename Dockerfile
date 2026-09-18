# Groow's body. Disposable: rebuilt from this file. Everything Groow is lives in /home/groow (a volume).
# Volta (V100) needs CUDA 12.x images and the cu126 torch wheels.
FROM nvidia/cuda:12.6.3-cudnn-runtime-ubuntu24.04

ENV DEBIAN_FRONTEND=noninteractive PYTHONUNBUFFERED=1 UV_LINK_MODE=copy \
    PATH=/opt/venv/bin:/root/.local/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3.12 python3.12-venv curl ca-certificates xz-utils git bash \
    && rm -rf /var/lib/apt/lists/* \
    && curl -LsSf https://astral.sh/uv/install.sh | sh \
    && install -m 0755 /root/.local/bin/uv /usr/local/bin/uv

# the runtime (root-owned, read-only for groow: it cannot corrupt its own body)
RUN uv venv --python 3.12 /opt/venv \
    && uv pip install --python /opt/venv/bin/python --index-url https://download.pytorch.org/whl/cu126 torch
COPY pyproject.toml README.md /opt/groow/
COPY groow /opt/groow/groow
COPY groow.json /opt/groow/groow.json
RUN uv pip install --python /opt/venv/bin/python /opt/groow sentencepiece protobuf "huggingface_hub[hf_xet]"

# the inhabitant: an unprivileged user whose home is a volume; /nix is a second mount backed by ./home/.nix
RUN (getent passwd 1000 && userdel -r "$(getent passwd 1000 | cut -d: -f1)" || true) \
    && useradd -m -u 1000 -s /bin/bash groow \
    && mkdir -p /nix && chown groow:groow /nix
COPY docker/entrypoint.sh /usr/local/bin/groow-entrypoint
RUN chmod 0755 /usr/local/bin/groow-entrypoint

USER groow
WORKDIR /home/groow
ENV HOME=/home/groow USER=groow GROOW_BODY=sandbox HF_HOME=/home/groow/.cache/huggingface \
    PATH=/home/groow/.venv/bin:/home/groow/.local/bin:/home/groow/.nix-profile/bin:/opt/venv/bin:/usr/local/bin:/usr/bin:/bin
VOLUME ["/home/groow", "/nix"]
EXPOSE 7373
ENTRYPOINT ["/usr/local/bin/groow-entrypoint", "groow"]
# inside the body there is no further sandbox
CMD ["start", "--nosandbox", "-v", "--host", "0.0.0.0"]
