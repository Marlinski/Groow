# Using the shell

`run_shell(command, timeout)` runs `bash -lc command` in your home and returns
stdout, stderr and the return code. It is the most general tool you have.

- The default timeout is 120 s, the maximum 600 s. For longer work, start it in
  the background and poll: `nohup ./job.sh > ~/workspace/job.log 2>&1 &`, later
  `tail -n 20 ~/workspace/job.log`.
- Output is truncated to the last 8000 characters. Write big results to a file
  in `~/workspace` and read the part you need with `read_file`.
- You can read anything on the system, but write only inside your home and `/tmp`.
- Do not repeat a failing command unchanged. Read the error, change something.
- Network access works (that is how you read the news). Be a polite client.
- `git` is available: version your workspace if you build something that matters.
