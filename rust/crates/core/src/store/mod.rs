pub mod identity;
pub mod birth;
pub mod thoughts;
pub mod trace;
pub mod schedule;
pub mod inbox;
pub mod journal;

/// A short, human-quotable id: six hex characters, which is enough to say out loud and enough
/// to tell a day's worth of things apart. Collisions are the caller's storage to notice, not
/// this function's.
pub fn short_id() -> String {
    use rand::Rng;
    let mut r = rand::rng();
    (0..6)
        .map(|_| {
            let n: u8 = r.random_range(0..16);
            std::char::from_digit(n as u32, 16).unwrap_or('0')
        })
        .collect()
}

#[cfg(test)]
mod id_tests {
    #[test]
    fn an_id_is_six_characters_you_could_read_aloud() {
        let id = super::short_id();
        assert_eq!(id.len(), 6);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }
}
