"""News: `news [--items N]` prints fresh headlines from your feeds (state/senses/feeds.txt), remembering what you have seen."""
import hashlib
import html
import json
import os
import re
import sys
import time
import urllib.request
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from pathlib import Path

DEFAULT_FEEDS = [
    "https://feeds.bbci.co.uk/news/world/rss.xml",
    "https://www.theguardian.com/world/rss",
    "https://feeds.npr.org/1001/rss.xml",
    "https://www.lemonde.fr/rss/une.xml",
    "https://hnrss.org/frontpage",
]
_TAG_RE = re.compile(r"<[^>]+>")
_WS_RE = re.compile(r"\s+")


def _dir() -> Path:
    d = Path(os.environ.get("GROOW_STATE", "state")) / "senses"
    d.mkdir(parents=True, exist_ok=True)
    return d


def _feeds() -> list:
    f = _dir() / "feeds.txt"
    if not f.exists():
        f.write_text("# one feed URL per line; edit freely (this is your file)\n" + "\n".join(DEFAULT_FEEDS) + "\n")
    return [l.strip() for l in f.read_text().splitlines() if l.strip() and not l.startswith("#")]


def _clean(s):
    return _WS_RE.sub(" ", html.unescape(_TAG_RE.sub(" ", s or ""))).strip()


def _date(s, fallback):
    for f in (lambda x: parsedate_to_datetime(x).date().isoformat(),
              lambda x: datetime.fromisoformat(x.replace("Z", "+00:00")).date().isoformat()):
        try:
            return f(s)
        except Exception:
            pass
    return fallback


def _find(el, *names):
    for n in names:
        for child in el:
            if child.tag.split("}")[-1] == n:
                return child.get("href") if (n == "link" and child.get("href")) else child.text
    return None


def _parse(xml_text: str, hint: str) -> list:
    root = ET.fromstring(xml_text)
    today = datetime.now(timezone.utc).date().isoformat()
    items, channel = [], root.find("channel")
    if channel is not None:
        source = _clean(_find(channel, "title")) or hint
        for it in channel.findall("item"):
            items.append({"source": source, "title": _clean(_find(it, "title")), "summary": _clean(_find(it, "description", "summary", "encoded")),
                          "link": (_find(it, "link") or "").strip(), "date": _date(_find(it, "pubDate", "date"), today)})
    else:
        source = _clean(_find(root, "title")) or hint
        for it in root:
            if it.tag.split("}")[-1] == "entry":
                items.append({"source": source, "title": _clean(_find(it, "title")), "summary": _clean(_find(it, "summary", "content")),
                              "link": (_find(it, "link") or "").strip(), "date": _date(_find(it, "published", "updated"), today)})
    return [i for i in items if i["title"]]


def _fetch(max_items: int = 8) -> dict:
    seen_p = _dir() / "news_seen.json"
    seen = json.loads(seen_p.read_text()) if seen_p.exists() else {}
    per_feed, errors, now = [], [], time.time()
    for url in _feeds():
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "groow/0.3 (+news skill)"})
            with urllib.request.urlopen(req, timeout=10) as r:
                raw = _parse(r.read().decode("utf-8", errors="replace"), url)
        except Exception as e:
            errors.append(f"{url}: {type(e).__name__}")
            continue
        fresh = []
        for d in raw:
            iid = hashlib.sha1((d["link"] or d["title"]).encode()).hexdigest()[:12]
            if iid in seen or len(d["summary"]) < 40:
                continue
            d["id"] = iid
            fresh.append(d)
        per_feed.append(fresh)
    picked, i = [], 0
    while len(picked) < max_items and any(len(f) > i for f in per_feed):
        for f in per_feed:
            if i < len(f) and len(picked) < max_items:
                picked.append(f[i])
        i += 1
    for d in picked:
        seen[d["id"]] = now
    cutoff = now - 30 * 86400
    seen_p.write_text(json.dumps({k: v for k, v in seen.items() if v > cutoff}))
    with (_dir() / "news_items.jsonl").open("a") as f:
        for d in picked:
            f.write(json.dumps({**d, "fetched_at": now}, ensure_ascii=False) + "\n")
    return {"date_today": datetime.now(timezone.utc).date().isoformat(), "items": picked, "feed_errors": errors}


def main(argv: list) -> dict:
    """news [--items N] [--json]: fresh headlines you have not seen, from your feeds."""
    if argv and argv[0] in ("-h", "--help"):
        return {"text": "usage: news [--items N] [--json]   fresh headlines from state/senses/feeds.txt (edit it to change sources)"}
    if argv and argv[0] == "--selftest":
        return {"ok": True}
    n, as_json = 8, False
    for i, a in enumerate(argv):
        if a == "--items" and i + 1 < len(argv):
            n = int(argv[i + 1])
        if a == "--json":
            as_json = True
    r = _fetch(n)
    if as_json:
        return r
    lines = [f"news of {r['date_today']} ({len(r['items'])} new items):", ""]
    for it in r["items"]:
        lines.append(f"- [{it['source']}, {it['date']}] {it['title']}\n  {it['summary'][:300]}\n  {it['link']}")
    if r["feed_errors"]:
        lines.append("\nfeeds that failed: " + ", ".join(r["feed_errors"]))
    return {"text": "\n".join(lines)}


CLI = {"news": "main"}
TESTS = [("news", {"argv": ["--selftest"]}, {"ok": True})]
