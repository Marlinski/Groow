# Your skills

A skill is two things kept together: a page saying how and when to use something, and the
command that does it. They live in `~/skills`, one directory each, and they are yours.

```
~/skills/news/
  SKILL.md        what it is, when to reach for it, how to use it
  scripts/news    the command itself
```

Your prompt lists the name and the description of each one, and nothing more. When you want to
know how to use one, read it:

```
cat ~/skills/news/SKILL.md
ls ~/skills
```

The commands themselves are on your `PATH`, so you run them in your shell like anything else:
`news --items 3`, not a tool call. Something that is not a tool will tell you so.

## Writing one

There is nothing to register and nobody to ask.

```
mkdir -p ~/skills/tides/scripts
cat > ~/skills/tides/scripts/tides <<'EOF'
#!/bin/sh
# tides <port>: the next high tide
curl -fsS "https://example.org/tides/$1" | head -20
EOF
chmod +x ~/skills/tides/scripts/tides
cp ~/skills/tides/scripts/tides ~/bin/tides
tides brest
```

Then write the page beside it, because in a week you will not remember why you made it:

```
cat > ~/skills/tides/SKILL.md <<'EOF'
---
name: tides
description: The next high tide at a port. Use when asked about tides or sailing times.
---

# Tides

`tides <port>` prints the next few high tides. The port is the short name, not the full one.
The feed is often slow; if it hangs, that is the feed and not you.
EOF
```

The name should be lowercase with hyphens and match the directory. The description is the only
part that will be in front of you at the start of every turn, so make it say what the skill
does *and when it is worth reaching for*.

Any language will do. It is run, not imported, so what matters is that it is executable and
that it exits non-zero when it fails.

## Why write the page at all

Because the more you use a skill, the less you will need it: what you practise ends up in your
weights. The page is for the skill you wrote last week and have not touched since, and for the
day something stops working and you need to remember what it was supposed to do.
