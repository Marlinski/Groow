# Groow in a sandbox. Volta (V100) needs CUDA 12.x images and the cu126 torch wheels.
FROM nvidia/cuda:12.6.3-cudnn-runtime-ubuntu24.04

ENV DEBIAN_FRONTEND=noninteractive PYTHONUNBUFFERED=1 \
    HF_HOME=/data/hf-cache UV_LINK_MODE=copy PATH=/opt/venv/bin:/root/.local/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends python3.12 python3.12-venv curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && curl -LsSf https://astral.sh/uv/install.sh | sh

WORKDIR /app
RUN uv venv --python 3.12 /opt/venv \
    && uv pip install --python /opt/venv/bin/python --index-url https://download.pytorch.org/whl/cu126 torch
COPY pyproject.toml README.md ./
COPY groow ./groow
RUN uv pip install --python /opt/venv/bin/python -e . sentencepiece protobuf "huggingface_hub[hf_xet]"

# everything Groow learns lives under /data (mounted from ./data on the host)
RUN mkdir -p /data/state /data/hf-cache && ln -s /data/state /app/state
VOLUME ["/data"]
ENTRYPOINT ["groow"]
CMD ["chat"]
