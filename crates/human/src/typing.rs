//! Typing a chat line: how long it takes a person from deciding to say
//! something to pressing Enter.

use crate::Noise;

/// One person's typing: a steady pace of their own, with hesitations.
pub struct Typist {
    noise: Noise,
    /// Median seconds per ordinary keystroke for this person.
    interval: f64,
}

impl Typist {
    pub fn new(seed: u64) -> Self {
        let mut noise = Noise::new(seed);
        // 45 to 85 words a minute at five keystrokes a word.
        let interval = noise.uniform(0.14, 0.27);
        Self { noise, interval }
    }

    /// From the decision to the chat box being open: finding T (or /).
    pub fn open_delay(&mut self) -> f64 {
        self.noise.lognormal(0.35, 0.35).clamp(0.15, 1.5)
    }

    /// From the chat box opening to Enter going down, for this text.
    pub fn time_to_type(&mut self, text: &str) -> f64 {
        let mut total = 0.0;
        let mut previous = ' ';
        for c in text.chars() {
            let mut keystroke = self.noise.lognormal(self.interval, 0.35);
            // Shifted characters and digits are a reach.
            if c.is_uppercase() || (!c.is_alphanumeric() && c != ' ' && c != '/') || c.is_ascii_digit() {
                keystroke *= 1.5;
            }
            // The same key twice is quick.
            if c == previous {
                keystroke *= 0.6;
            }
            // Now and then a pause before a word: thinking, or a typo fixed.
            if previous == ' ' && self.noise.chance(0.08) {
                keystroke += self.noise.uniform(0.3, 1.1);
            }
            total += keystroke.max(0.03);
            previous = c;
        }
        // A glance over it, then Enter.
        total + self.noise.lognormal(0.22, 0.4).clamp(0.08, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_takes_human_time() {
        let text = "hello there, how are you?";
        let mut fastest = f64::MAX;
        let mut slowest: f64 = 0.0;
        for seed in 0..200 {
            let t = Typist::new(seed).time_to_type(text);
            fastest = fastest.min(t);
            slowest = slowest.max(t);
        }
        // 25 characters: nobody under two seconds, nobody over twenty.
        assert!(fastest > 2.0, "fastest {fastest}");
        assert!(slowest < 20.0, "slowest {slowest}");
        // One person is consistent but never identical.
        let mut typist = Typist::new(5);
        let (a, b) = (typist.time_to_type(text), typist.time_to_type(text));
        assert!(a != b && (a / b) > 0.5 && (a / b) < 2.0);
        // Longer lines take longer.
        assert!(typist.time_to_type(&text.repeat(4)) > a.max(b));
    }
}
