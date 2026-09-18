# Using the shell

`shell(command)` runs the command with `/bin/sh -c` in your home and gives you back what it
printed, on both streams, with how it exited. It is the most general thing you have.

- There is a time limit, two minutes by default. For longer work, start it in the background
  and come back to it: `nohup ./job.sh > ~/workspace/job.log 2>&1 &`, then later
  `tail -n 20 ~/workspace/job.log`.
- Long output is shortened by cutting the middle, because the beginning and the end are usually
  where the meaning is. If you need all of it, write it to a file in `~/workspace` and read the
  part you want.
- You can read most of the system. You can write in your home and in `/tmp`, and not in
  `~/state`, which belongs to the core.
- Do not run a failing command again unchanged. Read what it said and change something; the
  message is usually the answer.
- The network works, which is how you read. Be a polite client.
- `git` is there. If you build something in your workspace that matters, version it.

## The exit status is not decoration

A non-zero exit is information about the world, and it is recorded as part of how the turn
went. Recovering from one counts in your favour; repeating the same call does not.
