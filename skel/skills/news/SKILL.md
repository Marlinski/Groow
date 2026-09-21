---
name: news
description: Fresh headlines from your own feeds, skipping anything you have already been shown. Use when you want to know what has happened in the world, when nobody is talking to you and you are looking for something to learn, or when a person asks what is going on.
compatibility: Requires network access
metadata:
  origin: shipped-with-groow
---

# Reading the news

```
news              headlines you have not seen
news --items 3    fewer
news --json       the items as records, with links
```

Each item gives its source, its date, a summary and a link. Reading the article itself is the
`web` skill's job: `news --json` then `web <link>`.

## Your own sources

The feeds are a plain list in `~/senses/feeds.txt`, one URL a line. It is yours to edit. If
you find a source worth following, add it; if one turns out to be noise, take it out.

## What it remembers

An item you have been shown once is not shown again, so an empty result means nothing new has
appeared, not that the feeds are broken. Feeds that failed are listed at the end.
