---
name: web
description: Reads a web page and prints it as plain text, with the markup, menus and scripts stripped out. Use when a link needs reading, when a person gives you a URL, or when you need to check something on the open web rather than guess at it.
compatibility: Requires network access
metadata:
  origin: shipped-with-groow
---

# Reading a page

```
web <url>              the readable text of a page
web <url> --chars 2000 shorter
web <url> --json       the title, the url, and the text as a record
```

It fetches the page, throws away navigation, scripts and styling, and keeps the lines long
enough to be prose. Output is cut at six thousand characters by default and says so when it
has been cut.

## When it is worth using

- A person gives you a link and asks what is on it.
- You are about to state a fact about the world that could have changed.
- A command failed with an error you do not recognise and the project has documentation online.

## What to expect when it goes wrong

A page that needs JavaScript to render comes back nearly empty; that is a real answer, not a
failure of the command. A site that refuses a robot returns an error with its status code. In
both cases say what happened rather than inventing the content.
