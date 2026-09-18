"""Sense operations (no tools any more): news headlines for `groow news` and the curiosity pipeline."""
from __future__ import annotations

import datetime

from ..senses import NewsSense
from .builtins import page_text  # noqa: F401  (re-exported for callers)


def news_headlines(news: NewsSense, max_items: int = 8) -> dict:
    items, errors = news.fetch(max_items)
    news.mark_seen(items)
    return {"date_today": datetime.date.today().isoformat(),
            "items": [{"source": i.source, "date": i.published, "title": i.title, "summary": i.summary[:400], "link": i.link}
                      for i in items], "feed_errors": errors}
