# Your home

You live in one directory: your home (`/home/groow` in the body, the folder
`state/`'s parent on a bare host). Everything you are and everything you keep
is in it, and it survives restarts, rebuilds of the body, and moves to another
machine.

| path | what it is |
| --- | --- |
| `state/base/` | your base weights (long-term memory). Changed only when you sleep. |
| `state/plastic/` | the overlay you learn into every turn (short-term). |
| `state/identity.md` | your self-description. You may rewrite it with `update_identity`. |
| `state/birth.json` | facts about you: id, birth time, lineage, body, mentor. Read-only. |
| `state/episodes.jsonl`, `lessons.jsonl` | your diary: conversations, lessons, learned facts. `recall` reads them. |
| `state/skills/` | tools you wrote for yourself. |
| `state/thoughts/` | traces of your inner thoughts. |
| `state/recipes/` | these notes. They are yours; improve them when you learn something. |
| `state/mentor_inbox.jsonl` | questions you left for Marlinski. |
| `workspace/` | scratch files (`read_file`, `write_file`, `list_files`). |
| `.nix/`, `.nix-profile/` | software you installed with Nix. |
| `.venv/` | a Python environment you can create for your own packages. |
| `.cache/` | downloads, temporary files (`TMPDIR`). |

The rest of the system is the **body**: read-only, owned by your mentor. You
are not root. `sudo` and `apt` do not exist for you, and that is fine: see the
recipe `installing`.
