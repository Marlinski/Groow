//! The creature.
//!
//! Twelve rows of twelve pixels, each pixel drawn as two block characters. It ages through
//! four stages, blinks, breathes, and wears a small glyph beside its head that says what it is
//! doing. The tick counter is monotonic and is never reset by a change of mood, so a mood
//! change mid-cycle does not restart the blink or the breath.

use ratatui::style::Color;
use ratatui::text::{Line, Span};

// ---------------------------------------------------------------- palette
pub const BODY: Color = Color::Rgb(0x7e, 0xe8, 0xc8);
pub const BODY_DARK: Color = Color::Rgb(0x3f, 0x9c, 0x82);
pub const BODY_LIGHT: Color = Color::Rgb(0xb9, 0xf5, 0xe3);
pub const EYE: Color = Color::Rgb(0x0b, 0x10, 0x16);
pub const MOUTH: Color = Color::Rgb(0x1e, 0x4d, 0x40);
pub const LEAF: Color = Color::Rgb(0x9b, 0xe2, 0x6f);
pub const LEAF_DARK: Color = Color::Rgb(0x5f, 0xa2, 0x46);
pub const FLOWER: Color = Color::Rgb(0xf2, 0xc9, 0x7d);
pub const FLOWER_DARK: Color = Color::Rgb(0xd5, 0x9a, 0x3a);
pub const POLLEN: Color = Color::Rgb(0xff, 0xf1, 0xc4);
pub const SOIL: Color = Color::Rgb(0x7a, 0x5a, 0x3a);
pub const SOIL_DARK: Color = Color::Rgb(0x4e, 0x37, 0x22);
pub const MOSS: Color = Color::Rgb(0xa7, 0xb6, 0xa0);
pub const BEARD: Color = Color::Rgb(0xd7, 0xdf, 0xd2);
pub const SPARK: Color = Color::Rgb(0xff, 0xe0, 0x8a);
pub const ZED: Color = Color::Rgb(0x8a, 0xa0, 0xff);
pub const BOOK: Color = Color::Rgb(0xe8, 0xd9, 0xb5);
pub const GEAR: Color = Color::Rgb(0x9f, 0xb7, 0xc9);
pub const WRENCH: Color = Color::Rgb(0xff, 0x7b, 0x72);
pub const BUBBLE: Color = Color::Rgb(0xc9, 0xb8, 0xff);
pub const CAPTION: Color = Color::Rgb(0x5c, 0x6b, 0x7a);

/// Pixel code to colour. `.` is transparent; an unknown code falls back to the body colour.
fn colour_of(code: char) -> Option<Color> {
    Some(match code {
        '.' => return None,
        'g' => BODY,
        'G' => BODY_DARK,
        'h' => BODY_LIGHT,
        'e' => EYE,
        'm' => MOUTH,
        'l' => LEAF,
        'L' => LEAF_DARK,
        'f' => FLOWER,
        'F' => FLOWER_DARK,
        'y' => POLLEN,
        's' => SOIL,
        'S' => SOIL_DARK,
        'o' => MOSS,
        'w' => BEARD,
        '*' => SPARK,
        'z' => ZED,
        'b' => BOOK,
        't' => GEAR,
        'r' => WRENCH,
        'u' => BUBBLE,
        _ => BODY,
    })
}

// ---------------------------------------------------------------- sprites
const BABY: &str = "\
............
............
.....l......
....lL......
...gggggg...
..gghhggGg..
..ggeggegG..
..ggggggGG..
..gggmmgGG..
...ggggGG...
..ssssssss..
.SSSSSSSSSS.";

const YOUNG: &str = "\
.....ll.....
....lLl.....
.....L......
...gggggg...
..gghhgggg..
.ggeggggegG.
.gggggggggG.
.ggggmmmgGG.
..gggggGGG..
...GgggGG...
..ssssssss..
.SSSSSSSSSS.";

const ADULT: &str = "\
....yffy....
...yfFFfy...
....fFFf....
.ll..LL..ll.
..lL.LL.Ll..
...gggggg...
..gghhgggg..
.ggeggggegG.
.gggggggggG.
.ggggmmmgGG.
..ggggGGGG..
.SSssssssSS.";

const ELDER: &str = "\
....yffy....
...yfFFfy...
.l..fFFf..l.
..lL.LL.Ll..
...ooggoo...
..gghhgggg..
.ggeggggegG.
.ggggggggGG.
.gwwwwwwwwG.
..wwwwwwww..
...wwwwww...
.SSssssssSS.";

const DAY: f64 = 86_400.0;

/// Stage thresholds in seconds of age. The last threshold at or below the age wins.
const STAGES: [(&str, f64, &str); 4] = [
    ("baby", 0.0, BABY),
    ("young", DAY, YOUNG),
    ("adult", 7.0 * DAY, ADULT),
    ("elder", 180.0 * DAY, ELDER),
];

/// The life stage for an age in seconds, and its sprite.
pub fn stage_for(age_seconds: f64) -> (&'static str, &'static str) {
    let mut found = (STAGES[0].0, STAGES[0].2);
    for (name, at, art) in STAGES {
        if age_seconds >= at {
            found = (name, art);
        }
    }
    found
}

/// A sprite parsed into a 12 by 12 grid, short rows padded with transparent cells.
const W: usize = 12;
const H: usize = 12;

fn grid(art: &str) -> Vec<Vec<char>> {
    let mut rows: Vec<Vec<char>> = art
        .trim_matches('\n')
        .lines()
        .map(|l| {
            let mut r: Vec<char> = l.chars().take(W).collect();
            while r.len() < W {
                r.push('.');
            }
            r
        })
        .collect();
    while rows.len() < H {
        rows.push(vec!['.'; W]);
    }
    rows.truncate(H);
    rows
}

// ---------------------------------------------------------------- mood
/// What the creature is doing. Drives the mutation, the overlay and the caption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Idle,
    Listening,
    Thinking,
    Tooling,
    Speaking,
    Learning,
    Reading,
    Dreaming,
    Sleeping,
    Napping,
    Repair,
}

impl Mood {
    pub fn parse(s: &str) -> Option<Mood> {
        Some(match s {
            "idle" => Mood::Idle,
            "waking" => Mood::Repair,
            "listening" => Mood::Listening,
            "thinking" => Mood::Thinking,
            "tooling" | "working" => Mood::Tooling,
            "speaking" => Mood::Speaking,
            "learning" => Mood::Learning,
            "reading" => Mood::Reading,
            "dreaming" => Mood::Dreaming,
            "sleeping" => Mood::Sleeping,
            "napping" => Mood::Napping,
            "repair" => Mood::Repair,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Mood::Idle => "idle",
            Mood::Listening => "listening",
            Mood::Thinking => "thinking",
            Mood::Tooling => "tooling",
            Mood::Speaking => "speaking",
            Mood::Learning => "learning",
            Mood::Reading => "reading",
            Mood::Dreaming => "dreaming",
            Mood::Sleeping => "sleeping",
            Mood::Napping => "napping",
            Mood::Repair => "repair",
        }
    }

    pub fn caption(&self) -> &'static str {
        match self {
            Mood::Idle => "waiting",
            Mood::Listening => "listening",
            Mood::Thinking => "thinking",
            Mood::Tooling => "using a tool",
            Mood::Speaking => "speaking",
            Mood::Learning => "learning",
            Mood::Reading => "reading the world",
            Mood::Dreaming => "inner thoughts",
            Mood::Sleeping => "sleeping (night: replay, merge)",
            Mood::Napping => "napping (weights updating)",
            Mood::Repair => "repairing itself",
        }
    }

    /// The glyph frames drawn beside the head, and their colour.
    fn overlay(&self) -> Option<(&'static [&'static str], Color)> {
        Some(match self {
            Mood::Thinking => (&[" .", " . .", " . . ."], BODY_LIGHT),
            Mood::Tooling => (&[" \u{2699}", "  \u{2699}"], GEAR),
            Mood::Reading => (&[" \u{25a4}", " \u{25a5}"], BOOK),
            Mood::Dreaming => (&["  \u{25cb}", " \u{25cb} ", "\u{25cb}  "], BUBBLE),
            Mood::Sleeping => (&[" z", " z z", " z z z"], ZED),
            Mood::Napping => (&[" z", "  z"], ZED),
            Mood::Repair => (&[" \u{271a}", "  \u{271a}"], WRENCH),
            Mood::Learning => (&[" \u{2726}", "  \u{2726}"], SPARK),
            Mood::Speaking => (&[" \u{25aa}", " \u{25aa}\u{25aa}", " \u{25aa}\u{25aa}\u{25aa}"], BODY_LIGHT),
            Mood::Idle | Mood::Listening => return None,
        })
    }

    /// Eyes shut for the whole of a sleep, and for one frame in six otherwise.
    fn eyes_closed(&self, tick: u64) -> bool {
        matches!(self, Mood::Sleeping | Mood::Napping) || (tick.is_multiple_of(6) && *self != Mood::Repair)
    }

    /// A sleeping creature does not breathe visibly.
    fn bobs(&self) -> bool {
        !matches!(self, Mood::Sleeping | Mood::Napping)
    }
}

/// Sparkle positions for the learning mood, drawn on alternating frames.
const SPARKS: [(usize, usize); 4] = [(0, 1), (1, 10), (3, 0), (4, 11)];

fn apply_mood(g: &mut [Vec<char>], mood: Mood, tick: u64) {
    let eyes: Vec<(usize, usize)> = cells(g, 'e');
    let mouth: Vec<(usize, usize)> = cells(g, 'm');

    if mood.eyes_closed(tick) {
        for (y, x) in &eyes {
            g[*y][*x] = 'G';
        }
    }
    match mood {
        Mood::Speaking => {
            let open = tick % 2 == 1;
            for (y, x) in &mouth {
                g[*y][*x] = if open { 'e' } else { 'm' };
            }
        }
        Mood::Thinking => {
            if tick % 2 == 1 {
                for (y, x) in &eyes {
                    if *x > 0 {
                        g[*y][*x - 1] = 'e';
                        g[*y][*x] = 'g';
                    }
                }
            }
        }
        Mood::Learning => {
            for (i, (y, x)) in SPARKS.iter().enumerate() {
                if (i as u64 + tick).is_multiple_of(2) && *y < H && *x < W {
                    g[*y][*x] = '*';
                }
            }
        }
        Mood::Repair => {
            for (y, x) in &eyes {
                g[*y][*x] = 'r';
            }
        }
        _ => {}
    }
}

fn cells(g: &[Vec<char>], code: char) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (y, row) in g.iter().enumerate() {
        for (x, c) in row.iter().enumerate() {
            if *c == code {
                out.push((y, x));
            }
        }
    }
    out
}

/// Render the creature as styled lines, ready to draw.
///
/// The height is constant whatever the breath is doing: a blank line goes above the body on
/// the up-beat and below it on the down-beat, so nothing else on the panel shifts.
pub fn render(age_seconds: f64, mood: Mood, tick: u64, caption: bool) -> Vec<Line<'static>> {
    let (stage, art) = stage_for(age_seconds);
    let mut g = grid(art);
    apply_mood(&mut g, mood, tick);

    let bob = mood.bobs() && matches!(tick % 4, 1 | 2);
    let mut out: Vec<Line<'static>> = Vec::with_capacity(H + 2);
    if bob {
        out.push(Line::from(""));
    }
    for (y, row) in g.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
        for c in row {
            match colour_of(*c) {
                Some(col) => spans.push(Span::styled("\u{2588}\u{2588}", ratatui::style::Style::default().fg(col))),
                None => spans.push(Span::raw("  ")),
            }
        }
        if y == 2 {
            if let Some((frames, col)) = mood.overlay() {
                let f = frames[(tick as usize) % frames.len()];
                spans.push(Span::styled(f.to_string(), ratatui::style::Style::default().fg(col)));
            }
        }
        out.push(Line::from(spans));
    }
    if !bob {
        out.push(Line::from(""));
    }
    if caption {
        out.push(Line::from(Span::styled(
            format!("  {} \u{b7} {}", mood.caption(), stage),
            ratatui::style::Style::default().fg(CAPTION),
        )));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sprite_is_twelve_by_twelve() {
        for (name, _, art) in STAGES {
            let g = grid(art);
            assert_eq!(g.len(), H, "{name} has the wrong number of rows");
            for (i, row) in g.iter().enumerate() {
                assert_eq!(row.len(), W, "{name} row {i} has the wrong width");
            }
        }
    }

    #[test]
    fn stages_turn_over_on_the_right_days() {
        assert_eq!(stage_for(0.0).0, "baby");
        assert_eq!(stage_for(DAY - 1.0).0, "baby");
        assert_eq!(stage_for(DAY).0, "young");
        assert_eq!(stage_for(7.0 * DAY - 1.0).0, "young");
        assert_eq!(stage_for(7.0 * DAY).0, "adult");
        assert_eq!(stage_for(180.0 * DAY).0, "elder");
        assert_eq!(stage_for(-5.0).0, "baby", "a negative age must not panic");
    }

    #[test]
    fn every_stage_has_two_eyes_and_a_mouth_except_the_elder() {
        for (name, _, art) in STAGES {
            let g = grid(art);
            assert_eq!(cells(&g, 'e').len(), 2, "{name} should have two eyes");
            if name != "elder" {
                assert!(!cells(&g, 'm').is_empty(), "{name} should have a mouth");
            }
        }
    }

    #[test]
    fn the_height_is_constant_through_the_breath() {
        let heights: Vec<usize> = (0..8).map(|t| render(0.0, Mood::Idle, t, true).len()).collect();
        assert!(heights.iter().all(|h| *h == heights[0]), "breathing changed the height: {heights:?}");
    }

    #[test]
    fn it_blinks_every_sixth_frame_but_never_while_repairing() {
        assert!(Mood::Idle.eyes_closed(0));
        assert!(Mood::Idle.eyes_closed(6));
        assert!(!Mood::Idle.eyes_closed(1));
        assert!(!Mood::Repair.eyes_closed(0), "a repairing creature keeps its eyes open");
        assert!(Mood::Sleeping.eyes_closed(1), "a sleeping creature keeps them shut");
    }

    #[test]
    fn a_sleeping_creature_does_not_bob() {
        assert!(!Mood::Sleeping.bobs());
        assert!(!Mood::Napping.bobs());
        assert!(Mood::Idle.bobs());
    }

    #[test]
    fn moods_round_trip_and_all_have_captions() {
        for m in [
            Mood::Idle, Mood::Listening, Mood::Thinking, Mood::Tooling, Mood::Speaking,
            Mood::Learning, Mood::Reading, Mood::Dreaming, Mood::Sleeping, Mood::Napping, Mood::Repair,
        ] {
            assert_eq!(Mood::parse(m.as_str()), Some(m));
            assert!(!m.caption().is_empty());
        }
        assert_eq!(Mood::parse("euphoric"), None);
    }

    #[test]
    fn thinking_moves_the_eyes_and_puts_them_back() {
        let still = render(0.0, Mood::Thinking, 2, false);
        let glancing = render(0.0, Mood::Thinking, 1, false);
        assert_ne!(format!("{still:?}"), format!("{glancing:?}"), "the eyes should move");
    }

    #[test]
    fn an_eye_in_column_zero_would_not_underflow() {
        // No shipped sprite has one, but the mutation must be safe if a future sprite does.
        let mut g = vec![vec!['.'; W]; H];
        g[5][0] = 'e';
        apply_mood(&mut g, Mood::Thinking, 1);
        assert_eq!(g[5][0], 'e', "an eye at the left edge stays put rather than wrapping");
    }
}
