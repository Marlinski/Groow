# Installing software

You cannot use `apt` or `sudo`. Install into your home instead; it persists.

## Nix (system tools, libraries, languages)

```
nix search nixpkgs ffmpeg            # find the package name
nix profile install nixpkgs#ffmpeg   # install into ~/.nix-profile/bin (already on your PATH)
nix profile list                     # what you installed
nix profile remove ffmpeg            # uninstall
```

Anything in nixpkgs works: `jq`, `ripgrep`, `sqlite`, `nodejs`, `go`, `gcc`,
`imagemagick`, `tesseract`, `pandoc`… Prefer Nix for binaries and libraries.

## Python packages

Your runtime under `/opt/venv` is read-only. Make your own environment once:

```
uv venv ~/.venv
uv pip install --python ~/.venv/bin/python requests beautifulsoup4
~/.venv/bin/python -c "import requests; print(requests.__version__)"
```

`~/.venv/bin` is first on your PATH, so `python` in `shell` is yours once the venv exists.

## Plain binaries

Download to `~/.local/bin` (on your PATH) and `chmod +x`. Build from source in
`~/workspace`. Temporary files go to `~/.cache/tmp` (`$TMPDIR`), because `/tmp`
may be small and non-executable.

## Rules of thumb

- Long installs: run in the background, `nohup nix profile install nixpkgs#x > ~/.cache/install.log 2>&1 &`, then check the log later.
- If something needs root, it is not for you: `ask`, or `propose_patch` against the Dockerfile.
- After installing something useful, write a skill that wraps it, so you do not have to remember the command line.
