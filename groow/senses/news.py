"""The news sense: pulls new items from RSS/Atom feeds using only the standard
library, strips markup, remembers what it has already seen.
"""
from __future__ import annotations

import hashlib
import html
import json
import re
import time
import urllib.request
import xml.etree.ElementTree as ET
from dataclasses import dataclass, asdict
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


@dataclass
class NewsItem:
    id: str
    source: str
    title: str
    summary: str
    link: str
    published: str          # ISO date (YYYY-MM-DD) when known, else the fetch date
    fetched_at: float

    @property
    def text(self) -> str:
        return f"{self.title}. {self.summary}".strip()


def _clean(s: str | None) -> str:
    if not s:
        return ""
    s = html.unescape(_TAG_RE.sub(" ", s))
    return _WS_RE.sub(" ", s).strip()


def _date(s: str | None, fallback: str) -> str:
    if not s:
        return fallback
    try:
        return parsedate_to_datetime(s).date().isoformat()
    except Exception:
        pass
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00")).date().isoformat()
    except Exception:
        return fallback


def _find(el, *names):
    for n in names:
        for child in el:
            tag = child.tag.split("}")[-1]
            if tag == n:
                if tag == "link" and child.get("href"):
                    return child.get("href")
                return child.text
    return None


def parse_feed(xml_text: str, source_hint: str) -> list[dict]:
    root = ET.fromstring(xml_text)
    today = datetime.now(timezone.utc).date().isoformat()
    items = []
    channel = root.find("channel")
    if channel is not None:                                   # RSS 2.0
        source = _clean(_find(channel, "title")) or source_hint
        for it in channel.findall("item"):
            items.append({"source": source, "title": _clean(_find(it, "title")),
                          "summary": _clean(_find(it, "description", "summary", "encoded")),
                          "link": (_find(it, "link") or "").strip(),
                          "published": _date(_find(it, "pubDate", "date"), today)})
    else:                                                     # Atom
        source = _clean(_find(root, "title")) or source_hint
        for it in root:
            if it.tag.split("}")[-1] != "entry":
                continue
            items.append({"source": source, "title": _clean(_find(it, "title")),
                          "summary": _clean(_find(it, "summary", "content")),
                          "link": (_find(it, "link") or "").strip(),
                          "published": _date(_find(it, "published", "updated"), today)})
    return [i for i in items if i["title"]]


class NewsSense:
    def __init__(self, state_dir: Path, feeds: list[str] | None = None, timeout: int = 10):
        self.dir = Path(state_dir) / "senses"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.seen_path = self.dir / "news_seen.json"
        self.items_path = self.dir / "news_items.jsonl"
        self.feeds = feeds or DEFAULT_FEEDS
        self.timeout = timeout
        self.seen: dict[str, float] = json.loads(self.seen_path.read_text()) if self.seen_path.exists() else {}

    def _fetch_feed(self, url: str) -> list[dict]:
        req = urllib.request.Request(url, headers={"User-Agent": "groow/0.2 (+news sense)"})
        with urllib.request.urlopen(req, timeout=self.timeout) as r:
            return parse_feed(r.read().decode("utf-8", errors="replace"), source_hint=url)

    def fetch(self, max_items: int = 8, min_summary_chars: int = 40) -> tuple[list[NewsItem], list[str]]:
        """New, unseen items spread across feeds (round-robin), plus per-feed errors."""
        per_feed: list[list[NewsItem]] = []
        errors: list[str] = []
        now = time.time()
        for url in self.feeds:
            try:
                raw = self._fetch_feed(url)
            except Exception as e:
                errors.append(f"{url}: {type(e).__name__}: {e}")
                continue
            fresh = []
            for d in raw:
                iid = hashlib.sha1((d["link"] or d["title"]).encode()).hexdigest()[:12]
                if iid in self.seen or len(d["summary"]) < min_summary_chars:
                    continue
                fresh.append(NewsItem(id=iid, fetched_at=now, **d))
            per_feed.append(fresh)
        picked: list[NewsItem] = []
        i = 0
        while len(picked) < max_items and any(per_feed):
            for lst in per_feed:
                if i < len(lst) and len(picked) < max_items:
                    picked.append(lst[i])
            i += 1
            if all(i >= len(lst) for lst in per_feed):
                break
        return picked, errors

    def mark_seen(self, items: list[NewsItem]) -> None:
        for it in items:
            self.seen[it.id] = it.fetched_at
            with self.items_path.open("a") as f:
                f.write(json.dumps(asdict(it), ensure_ascii=False) + "\n")
        # forget ids older than 30 days so the file does not grow forever
        cutoff = time.time() - 30 * 86400
        self.seen = {k: v for k, v in self.seen.items() if v > cutoff}
        self.seen_path.write_text(json.dumps(self.seen))

    def recent(self, n: int = 20) -> list[dict]:
        if not self.items_path.exists():
            return []
        lines = self.items_path.read_text().splitlines()[-n:]
        return [json.loads(l) for l in lines if l.strip()]
