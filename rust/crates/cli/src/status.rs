//! What `groow status` looks like to a person.
//!
//! The command is one request over the socket and nothing else; this is only how the answer is
//! drawn. `--json` prints the reply itself, which is what a script wants and what every other
//! command prints. Nothing here asks the core for anything extra: if a line is not in the
//! reply, it is not on the box.

use serde_json::Value;

/// The narrowest the box is allowed to be, so it does not jump about between calls.
const MIN_WIDTH: usize = 52;

/// Draw the reply to a `status` request.
pub fn box_of(v: &Value) -> String {
    let rows = rows(v);
    let label = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let age = v.get("age").and_then(|a| a.as_str()).unwrap_or("").to_string();

    let body: Vec<String> =
        rows.iter().map(|(k, t)| format!("  {k:label$}   {t}", label = label)).collect();
    let inner = body
        .iter()
        .map(|l| l.chars().count())
        .chain(std::iter::once(age.chars().count() + 12))
        .max()
        .unwrap_or(MIN_WIDTH)
        .max(MIN_WIDTH)
        + 2;

    let mut out = String::new();
    // The header carries the age, because it is the one number that is true of the creature
    // rather than of this moment.
    let head = if age.is_empty() { String::new() } else { format!(" {age} old ─") };
    let dashes = inner.saturating_sub(head.chars().count() + 8);
    out.push_str(&format!("╭─ groow {}{}╮\n", "─".repeat(dashes), head));
    out.push_str(&format!("│{}│\n", " ".repeat(inner)));
    for line in &body {
        let pad = inner.saturating_sub(line.chars().count());
        out.push_str(&format!("│{line}{}│\n", " ".repeat(pad)));
    }
    out.push_str(&format!("│{}│\n", " ".repeat(inner)));
    out.push_str(&format!("╰{}╯", "─".repeat(inner)));
    out
}

/// The same rows every time, so the box does not change shape while you watch it.
fn rows(v: &Value) -> Vec<(&'static str, String)> {
    vec![
        ("mood", mood(v)),
        ("doing", doing(v)),
        ("brain", brain(v)),
        ("turns", turns(v)),
        ("waiting", waiting(v)),
        ("thoughts", thoughts(v)),
    ]
}

fn mood(v: &Value) -> String {
    let m = str_of(v, "mood").unwrap_or_else(|| "?".into());
    let f = v.get("feeling");
    let tone = f.and_then(|f| f.get("tone")).and_then(|t| t.as_str()).unwrap_or("");
    let pain = f.and_then(|f| f.get("pain")).and_then(|p| p.as_f64()).unwrap_or(0.0);
    let joy = f.and_then(|f| f.get("pleasure")).and_then(|p| p.as_f64()).unwrap_or(0.0);
    let mut s = if tone.is_empty() { m } else { format!("{m}, {tone}") };
    // Only when there is something to say: an even day should not be a row of zeroes.
    if pain > 0.0 || joy > 0.0 {
        s.push_str(&format!(" (pleasure {joy:.2}, pain {pain:.2})"));
    }
    s
}

fn doing(v: &Value) -> String {
    let queue = num(v, "queue");
    let waiting = match queue {
        0 => String::new(),
        1 => " · one message waiting".into(),
        n => format!(" · {n} messages waiting"),
    };
    let what = if let Some(n) = str_of(v, "napping") {
        format!("napping ({n})")
    } else if let Some(t) = str_of(v, "turn") {
        format!("a turn, {t}")
    } else if v.get("busy").and_then(|b| b.as_bool()).unwrap_or(false) {
        "thinking".into()
    } else {
        "nothing just now".into()
    };
    format!("{what}{waiting}")
}

fn brain(v: &Value) -> String {
    match v.get("brain").and_then(|b| b.as_bool()) {
        Some(true) => "loaded and answering".into(),
        Some(false) => "not loaded yet, so a turn would fail".into(),
        None => "unknown".into(),
    }
}

fn turns(v: &Value) -> String {
    let n = num(v, "turns");
    let rate = v.get("answer_rate").and_then(|r| r.as_f64());
    let so_far = match n {
        0 => "none yet".to_string(),
        1 => "one so far".to_string(),
        n => format!("{n} so far"),
    };
    match rate {
        Some(r) if n > 0 => format!("{so_far}, {:.0}% answered", r * 100.0),
        _ => so_far,
    }
}

fn waiting(v: &Value) -> String {
    match num(v, "open_questions") {
        0 => "nothing; it has not asked you anything".into(),
        1 => "one question for you (`groow inbox`)".into(),
        n => format!("{n} questions for you (`groow inbox`)"),
    }
}

fn thoughts(v: &Value) -> String {
    match num(v, "thoughts") {
        0 => "none of its own just now".into(),
        1 => "one of its own (`groow thoughts`)".into(),
        n => format!("{n} of its own (`groow thoughts`)"),
    }
}

/// A string field, treating null and empty as absent, which is how the core sends "no".
fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())
}

fn num(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn idle() -> Value {
        json!({
            "brain": true, "busy": false, "age": "22h 16m", "napping": null,
            "answer_rate": 1, "idle_streak": 1, "queue": 0, "thoughts": 0,
            "feeling": {"pain": 0, "tone": "even", "pleasure": 0},
            "open_questions": 1, "turn": null, "mood": "idle",
            "born": 1789731096.238, "turns": 38
        })
    }

    #[test]
    fn the_box_says_what_the_json_said() {
        let s = box_of(&idle());
        assert!(s.contains("22h 16m old"), "{s}");
        assert!(s.contains("idle, even"), "{s}");
        assert!(s.contains("nothing just now"), "{s}");
        assert!(s.contains("loaded and answering"), "{s}");
        assert!(s.contains("38 so far, 100% answered"), "{s}");
        assert!(s.contains("one question for you"), "{s}");
    }

    #[test]
    fn every_line_of_the_box_is_the_same_width() {
        // Including the ones with box-drawing characters, which are not one byte each.
        for v in [idle(), json!({}), json!({"mood": "thinking", "turn": "01789810553254-0000",
                                            "busy": true, "queue": 3, "age": "3d 4h"})] {
            let s = box_of(&v);
            let widths: Vec<usize> = s.lines().map(|l| l.chars().count()).collect();
            assert!(widths.windows(2).all(|w| w[0] == w[1]), "{s}\n{widths:?}");
        }
    }

    #[test]
    fn a_missing_field_does_not_make_a_hole_in_it() {
        let s = box_of(&json!({}));
        assert!(s.contains("unknown"), "the brain is not assumed to be up: {s}");
        assert!(s.contains("none yet"), "{s}");
    }

    #[test]
    fn what_it_is_doing_comes_before_what_is_waiting_for_it() {
        let s = box_of(&json!({"turn": "01789-0000", "queue": 2, "mood": "thinking"}));
        assert!(s.contains("a turn, 01789-0000 · 2 messages waiting"), "{s}");
        let s = box_of(&json!({"napping": "since 03:00", "mood": "asleep"}));
        assert!(s.contains("napping (since 03:00)"), "{s}");
    }

    #[test]
    fn a_feeling_is_only_spelled_out_when_there_is_one() {
        let s = box_of(&idle());
        assert!(!s.contains("pain 0"), "an even day is not a row of zeroes: {s}");
        let hurt = json!({"mood": "sore", "feeling": {"tone": "low", "pain": 0.4, "pleasure": 0.1}});
        assert!(box_of(&hurt).contains("(pleasure 0.10, pain 0.40)"));
    }
}
