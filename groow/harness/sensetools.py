"""Sense tools: how Groow reaches into the world and learns grounded facts from it.

    news_headlines   fresh items from the configured feeds (dated, sourced)
    read_article     fetch a web page and return its readable text
    learn_fact       drill a question -> sourced answer into the weights and file it as a lesson
"""
from __future__ import annotations

import html
import re
import urllib.request

from ..brain import build_sample
from ..learning import Learner, REHEARSAL_SYSTEM
from ..senses import NewsSense
from .registry import ToolRegistry

ANSWER_WEIGHTS = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}
_STRIP = re.compile(r"<(script|style|nav|header|footer|aside)[^>]*>.*?</\1>", re.DOTALL | re.IGNORECASE)
_TAGS = re.compile(r"<[^>]+>")
_WS = re.compile(r"[ \t\r\f\v]+")
_NL = re.compile(r"\n\s*\n+")


def page_text(url: str, timeout: int = 15, max_chars: int = 6000) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": "groow/0.2 (+reader)"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        raw = r.read(2_000_000).decode("utf-8", errors="replace")
    title = re.search(r"<title[^>]*>(.*?)</title>", raw, re.DOTALL | re.IGNORECASE)
    body = _STRIP.sub(" ", raw)
    body = re.sub(r"</(p|div|h\d|li|br|tr)>", "\n", body, flags=re.IGNORECASE)
    text = html.unescape(_TAGS.sub(" ", body))
    text = _NL.sub("\n", _WS.sub(" ", text)).strip()
    # keep the densest part: drop very short lines (menus, captions)
    lines = [l.strip() for l in text.splitlines() if len(l.strip()) > 60]
    text = "\n".join(lines) or text
    return {"url": url, "title": html.unescape(title.group(1)).strip() if title else "",
            "chars": len(text), "text": text[:max_chars], "truncated": len(text) > max_chars}


def make_sense_tools(learner: Learner, news: NewsSense, passes: int = 3) -> ToolRegistry:
    reg = ToolRegistry()
    brain, memory = learner.brain, learner.memory

    @reg.tool(group="sense")
    def news_headlines(max_items: int = 8) -> dict:
        """Fresh news items you have not seen yet, from your configured feeds. Each has a source,
        a date, a summary and a link you can pass to read_article.

        Args:
            max_items: how many items to return (spread across feeds)
        """
        items, errors = news.fetch(max_items)
        news.mark_seen(items)
        return {"date_today": __import__("datetime").date.today().isoformat(),
                "items": [{"source": i.source, "date": i.published, "title": i.title,
                           "summary": i.summary[:400], "link": i.link} for i in items],
                "feed_errors": errors}

    @reg.tool(group="sense")
    def read_article(url: str, max_chars: int = 6000) -> dict:
        """Fetch a web page and return its readable text (menus and scripts removed).

        Args:
            url: the page to read
            max_chars: truncate the text after this many characters
        """
        return page_text(url, max_chars=max_chars)

    @reg.tool(group="sense", executor="gpu")
    def learn_fact(question: str, answer: str, source: str) -> dict:
        """Learn a dated, sourced fact into your weights: drills question -> answer for a few steps and files
        it as a lesson. Use the source's own wording for the answer and include the date; never invent.

        Args:
            question: the question this fact answers
            answer: the answer in the source's wording, including when it happened
            source: where it came from (publication and date, or URL)
        """
        text = f"{answer.strip()} (Source: {source.strip()}.)"
        msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}, {"role": "user", "content": question},
                {"role": "assistant", "content": text}]
        s = build_sample(brain.tok, msgs, ANSWER_WEIGHTS, max_len=learner.cfg.train_max_len)
        before = brain.sample_loss(s)
        for _ in range(passes):
            brain.sft_step([s] + learner._rehearsal(1))
        after = brain.sample_loss(s)
        memory.add_lesson(f"fact: {question[:80]}", text, {"kind": "fact", "source": source, "question": question,
                                                             "loss_before": round(before, 3), "loss_after": round(after, 3)})
        memory.log("learn_fact", question=question[:80], loss_before=before, loss_after=after, step=brain.meta["steps"])
        return {"question": question, "loss_before": round(before, 3), "loss_after": round(after, 3), "steps": passes}

    return reg
