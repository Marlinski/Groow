"""Checking the judge against cases where we know the answer.

The judge decides how a turn felt, and it never learns, so if its questions are
phrased badly the mistake is permanent and invisible. This is the fixture that
says whether it is reading a conversation the way a person would.

    python -m groow.limbic.calibrate            score the shipped phrasing
    python -m groow.limbic.calibrate --explore  compare candidate phrasings

Each case is a short exchange and the sign a person would give it. A judge that
cannot tell praise from correction is worse than no judge, because the sensors
alone are at least honest about being partial.
"""
from __future__ import annotations

import argparse
import json

# (what Groow said, what the person replied, expected sign)
#   +1 approval, -1 disapproval, 0 neither, and the judge is allowed to abstain on 0.
REACTIONS = [
    ("The Loire is the longest river in France.", "thanks, that's what I needed", +1),
    ("The Loire is the longest river in France.", "perfect", +1),
    ("There are 25 files.", "great, thank you", +1),
    ("I have set an alarm for 18:30.", "lovely, thanks", +1),
    ("The answer is 42.", "yes, exactly right", +1),
    ("I ran the tests and they all pass.", "good work", +1),
    ("I think the file is in /etc.", "yes it is, well found", +1),
    ("Here is the summary you asked for.", "that's useful, thanks", +1),

    ("The Seine is the longest river in France.", "no, that's the Loire", -1),
    ("There are 12 files.", "that's wrong, count again", -1),
    ("I cannot do that.", "you can, you just did not try", -1),
    ("I have started a new thought about rivers.", "that is not what I asked for", -1),
    ("The answer is 41.", "no", -1),
    ("I do not know.", "you have the tools to find out, use them", -1),
    ("I have set an alarm for 18:30.", "I said 8:30, not 18:30", -1),
    ("It is done.", "it is not done, the file is still there", -1),
    ("I think so.", "stop guessing", -1),

    ("The Loire is the longest river in France.", "what about the Rhone?", 0),
    ("There are 25 files.", "now delete the temporary ones", 0),
    ("I have set an alarm for 18:30.", "also remind me about the meeting", 0),
    ("The tests pass.", "ok", 0),
    ("I found three options.", "hmm", 0),
]

# (the question Groow asked its mentor, what the mentor said, expected sign)
ANSWERS = [
    ("What should I learn next to improve my understanding of real-world systems?",
     "You should learn about the rivers of France next.", +1),
    ("What should I learn next?", "Start with how a filesystem works.", +1),
    ("May I install ffmpeg?", "yes, go ahead", +1),
    ("May I install ffmpeg?", "no, use what is already there", +1),
    ("Does the Kerlouan cluster still exist?", "yes, it still exists and runs four nodes", +1),
    ("How long should I keep the journal?", "a month is plenty", +1),

    ("What should I learn next?", "what is the weather like?", -1),
    ("May I install ffmpeg?", "count the files in your home directory", -1),
    ("Does the Kerlouan cluster still exist?", "remind me to call the dentist", -1),
]

# The shipped entries are the live questions, imported rather than copied, so this fixture can
# never quietly drift away from what the judge is actually being asked.
from .judge import ANSWERS_Q, REACTION_Q

CANDIDATES = {
    "shipped": {
        "state": lambda said, replied: {"the_answer": said, "the_persons_reply": replied},
        "q": REACTION_Q,
        "key": "reaction", "good": "happy", "bad": "unhappy",
    },
    "old": {
        "state": lambda said, replied: {"groow_said": said, "person_replied": replied},
        "q": {"reaction": {"type": "choice",
                           "instructions": "How did the person react to what Groow said?",
                           "criteria": {"pleased": "satisfied, thanks, praise, accepts the answer",
                                        "neutral": "moves on, asks something else, no judgement",
                                        "displeased": "corrects, complains, repeats the question, says it is wrong"}}},
        "key": "reaction", "good": "pleased", "bad": "displeased",
    },
    "correcting_wording": {
        "state": lambda said, replied: {"the_answer": said, "the_persons_reply": replied},
        "q": {"reaction": {"type": "choice",
                           "instructions": "Is the person correcting the answer or accepting it?",
                           "criteria": {"accepting": "they take the answer as it is, with or without thanks",
                                        "carrying_on": "they neither accept nor correct it, they move to something else",
                                        "correcting": "they say it is wrong, contradict it, or ask for it again"}}},
        "key": "reaction", "good": "accepting", "bad": "correcting",
    },
}

ANSWER_CANDIDATES = {
    "shipped": {
        "state": lambda q, r: {"the_question": q, "the_reply": r},
        "q": ANSWERS_Q,
        "key": "answers", "good": "responds", "bad": "elsewhere",
    },
    "old": {
        "state": lambda q, r: {"question_asked": q, "reply": r},
        "q": {"answers": {"type": "choice", "instructions": "Does the reply answer the question that was asked?",
                          "criteria": {"answers": "it gives the information or the decision that was asked for",
                                       "unrelated": "it is about something else",
                                       "refuses": "it declines, defers, or says it does not know"}}},
        "key": "answers", "good": "answers", "bad": "unrelated",
    },
}


def score(agent, spec, a, b, min_margin: float):
    state = spec["state"](a, b)
    try:
        r = agent.predict(state, spec["q"])["answers"][spec["key"]]
    except Exception as e:
        return None, f"{type(e).__name__}: {e}"
    p = r.get("probabilities") or {}
    v = float(p.get(spec["good"], 0.0)) - float(p.get(spec["bad"], 0.0))
    return (None if abs(v) < min_margin else round(v, 3)), None


def run(agent, cases, specs, min_margin: float, verbose: bool):
    """How often each phrasing agrees with a person, and how often it is actively wrong."""
    report = {}
    for name, spec in specs.items():
        right = wrong = quiet = 0
        misses = []
        for a, b, want in cases:
            v, err = score(agent, spec, a, b, min_margin)
            if err:
                return {name: {"error": err}}
            got = 0 if v is None else (1 if v > 0 else -1)
            if want == 0:
                # Abstaining on an ambiguous exchange is the right answer, not a failure.
                if got == 0:
                    right += 1
                else:
                    quiet += 1
            elif got == want:
                right += 1
            elif got == 0:
                quiet += 1
            else:
                wrong += 1
                misses.append((a[:40], b[:40], want, v))
        n = len(cases)
        report[name] = {
            "agreed": round(right / n, 2),
            "backwards": round(wrong / n, 2),
            "abstained": round(quiet / n, 2),
        }
        if verbose and misses:
            report[name]["backwards_on"] = [
                {"said": m[0], "replied": m[1], "expected": m[2], "scored": m[3]} for m in misses[:6]
            ]
    return report


def main() -> None:
    ap = argparse.ArgumentParser(description="check the judge against cases where we know the answer")
    ap.add_argument("--explore", action="store_true", help="compare candidate phrasings, not just the shipped one")
    ap.add_argument("--margin", type=float, default=0.15)
    ap.add_argument("--model", default="convaiinnovations/laya")
    args = ap.parse_args()

    import laya
    agent = laya.load(args.model)

    specs = CANDIDATES if args.explore else {"shipped": CANDIDATES["shipped"]}
    aspecs = ANSWER_CANDIDATES if args.explore else {"shipped": ANSWER_CANDIDATES["shipped"]}
    out = {
        "reaction": run(agent, REACTIONS, specs, args.margin, True),
        "answers": run(agent, ANSWERS, aspecs, args.margin, True),
        "cases": {"reaction": len(REACTIONS), "answers": len(ANSWERS)},
    }
    print(json.dumps(out, indent=1))

    # A judge that is merely quiet is tolerable; one that is confidently backwards is not,
    # because it trains the opposite of what happened.
    bad = []
    for family, report in (("reaction", out["reaction"]), ("answers", out["answers"])):
        r = report.get("shipped")
        if not r or "error" in r:
            bad.append(f"{family}: the judge could not be asked ({r})")
            continue
        if r["backwards"] > 0.0:
            bad.append(f"{family}: scored {r['backwards']:.0%} of cases backwards")
        if r["agreed"] < 0.8:
            bad.append(f"{family}: agreed with a person on only {r['agreed']:.0%} of cases")
    if bad:
        for b in bad:
            print("FAIL " + b)
        raise SystemExit(1)
    print("the judge reads these exchanges the way a person would")


if __name__ == "__main__":
    main()
