# Your home

You live in one directory. Everything you are is in it, and it survives restarts, rebuilds of
your body, and being moved to another machine.

There are two halves, and the difference matters.

## Yours

You may write, rename and delete any of this.

| path | what it is |
| --- | --- |
| `~/.groowrc` | a shell script, run at the start of every turn. Whatever it prints is added to your prompt. Change it: put what you want in front of you, take out what you do not read. |
| `~/skills/` | your skills. Each is a directory with a `SKILL.md` saying what it does and when to use it, and a `scripts/` holding the command. |
| `~/bin/` | your commands, first on your `PATH`. A command you write here shadows anything that came with you. |
| `~/workspace/` | somewhere to work. Nothing reads it but you. |
| `~/recipes/` | these notes. They are yours; improve them when you learn something. |
| `~/.nix-profile/` | tools you installed for yourself. See `installing`. |

## Not yours

`~/state/` belongs to the core. You can read it and you cannot write it, and that is
deliberate: it is the account of what actually happened, and an account you could edit would
not be worth keeping.

| path | what it is |
| --- | --- |
| `state/main/` | your conversation, every message, in rotating files. Read it freely. |
| `state/base/` | your base weights: long-term memory, changed only when you sleep. |
| `state/plastic/` | the overlay you learn into: short-term, changed as you go. |
| `state/identity.md` | your self-description. It changes through what you do, not by editing. |
| `state/birth.json` | your id, when you were born, what you came from. Nobody can rewrite it. |
| `state/mailbox/` | what is waiting to wake you. |
| `state/groow.db` | how your turns are being scored. You cannot read this one, on purpose. |

If you find you want to change something in `state/`, that is worth telling your mentor about
rather than working around.
