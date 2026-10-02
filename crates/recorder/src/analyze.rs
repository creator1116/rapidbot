//! Fits `MouseProfile` parameters to a recording.
//!
//! Mouse events are grouped into strokes (movement with no gap longer than
//! [`STROKE_GAP`]). From those:
//! - Fitts' law constants: regression of stroke duration on
//!   `log2(amplitude / width + 1)`;
//! - settle time: the pause between a large stroke and a small one that
//!   follows closely (a correction);
//! - fidget interval: how often small isolated movements happen;
//! - key statistics (tap lengths), reported for reference.
//!
//! Traits that cannot be read from input alone (reaction time to an
//! on-screen event, intended target, tremor) keep their defaults.

use std::fmt::Write as _;

use rapidbot_human::MouseProfile;

/// Seconds of silence that end a stroke.
const STROKE_GAP: f64 = 0.08;

struct Stroke {
    start: f64,
    end: f64,
    dx: f64,
    dy: f64,
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    Some(values[values.len() / 2])
}

pub fn run(path: &str, sensitivity: f64) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    // Same maths as MouseHandler.turnPlayer.
    let ss = sensitivity * 0.6f32 as f64 + 0.2f32 as f64;
    let deg_per_count = ss * ss * ss * 8.0 * 0.15f32 as f64;

    let mut strokes: Vec<Stroke> = Vec::new();
    let mut key_down_at: std::collections::HashMap<String, f64> = Default::default();
    let mut taps: std::collections::HashMap<String, Vec<f64>> = Default::default();
    let mut focused_time = 0.0;
    let mut focus_since: Option<f64> = None;
    let mut last_t = 0.0;

    for line in text.lines() {
        let mut parts = line.split(',');
        let (Some(t), Some(kind), Some(a), Some(b)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(t) = t.parse::<f64>() else { continue };
        let t = t / 1e6;
        last_t = t;
        match kind {
            "m" => {
                let (dx, dy) = (a.parse::<f64>().unwrap_or(0.0), b.parse::<f64>().unwrap_or(0.0));
                match strokes.last_mut() {
                    Some(s) if t - s.end <= STROKE_GAP => {
                        s.end = t;
                        s.dx += dx;
                        s.dy += dy;
                    }
                    _ => strokes.push(Stroke { start: t, end: t, dx, dy }),
                }
            }
            "k" => {
                if b == "1" {
                    // Key repeat sends more "down" events; keep the first.
                    key_down_at.entry(a.to_owned()).or_insert(t);
                } else if let Some(down) = key_down_at.remove(a) {
                    taps.entry(a.to_owned()).or_default().push(t - down);
                }
            }
            "f" => {
                if a == "1" {
                    focus_since = Some(t);
                } else if let Some(since) = focus_since.take() {
                    focused_time += t - since;
                }
            }
            _ => {}
        }
    }
    if let Some(since) = focus_since {
        focused_time += last_t - since;
    }
    if strokes.len() < 50 {
        return Err(format!("only {} mouse strokes in the recording; play for longer", strokes.len()));
    }

    let mut profile = MouseProfile::default();
    let amplitude = |s: &Stroke| s.dx.hypot(s.dy) * deg_per_count;

    // Fitts' law: least squares of duration against the index of difficulty.
    let points: Vec<(f64, f64)> = strokes
        .iter()
        .filter(|s| amplitude(s) >= 3.0 && s.end > s.start)
        .map(|s| ((amplitude(s) / profile.target_width + 1.0).log2(), s.end - s.start))
        .collect();
    let mut fitts_note = "too few large strokes; kept defaults".to_owned();
    if points.len() >= 30 {
        let n = points.len() as f64;
        let (mx, my) = (points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n);
        let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
        let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        if sxx > 0.0 {
            let b = sxy / sxx;
            let a = my - b * mx;
            if b > 0.0 {
                profile.fitts_a = a.clamp(0.0, 0.4);
                profile.fitts_b = b.clamp(0.02, 0.4);
                fitts_note = format!("fitted on {} strokes", points.len());
            } else {
                fitts_note = "duration did not grow with distance; kept defaults".to_owned();
            }
        }
    }

    // Corrections: a small stroke soon after a much larger one.
    let mut settles: Vec<f64> = strokes
        .windows(2)
        .filter(|w| amplitude(&w[0]) >= 5.0 && amplitude(&w[1]) < amplitude(&w[0]) * 0.3)
        .map(|w| w[1].start - w[0].end)
        .filter(|gap| *gap < 0.5)
        .collect();
    let corrections = settles.len();
    if let Some(m) = median(&mut settles) {
        profile.settle_median = m.clamp(0.04, 0.4);
    }

    // Fidgets: small strokes with quiet on both sides.
    let fidgets = strokes
        .windows(3)
        .filter(|w| amplitude(&w[1]) < 1.5 && w[1].start - w[0].end > 0.5 && w[2].start - w[1].end > 0.5)
        .count();
    if fidgets > 0 && focused_time > 60.0 {
        profile.fidget_interval = (focused_time / fidgets as f64).clamp(1.0, 120.0);
    }

    let mut speeds: Vec<f64> = strokes
        .iter()
        .filter(|s| s.end - s.start > 0.03)
        .map(|s| amplitude(s) / (s.end - s.start))
        .collect();
    let mut report = String::new();
    let _ = writeln!(report, "// {} strokes over {:.0} s in game ({:.3} deg/count)", strokes.len(), focused_time, deg_per_count);
    let _ = writeln!(report, "// Fitts: a = {:.3} s, b = {:.3} s/bit ({fitts_note})", profile.fitts_a, profile.fitts_b);
    let _ = writeln!(report, "// corrections seen: {corrections}; settle median {:.3} s", profile.settle_median);
    let _ = writeln!(report, "// fidgets: {fidgets}; one every {:.1} s", profile.fidget_interval);
    if let Some(m) = median(&mut speeds) {
        let _ = writeln!(report, "// median mean stroke speed {m:.0} deg/s");
    }
    let mut keys: Vec<_> = taps.iter_mut().collect();
    keys.sort_by(|a, b| a.0.cmp(b.0));
    for (key, durations) in keys {
        let count = durations.len();
        if let Some(m) = median(durations) {
            let _ = writeln!(report, "// key {key}: {count} presses, median hold {m:.3} s");
        }
    }
    report.push_str(&serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_fitts_constants_from_synthetic_strokes() {
        // Strokes generated with a = 0.10, b = 0.09 at 125 Hz.
        let mut csv = String::from("0,f,1,0\n");
        let mut t = 1_000_000u64;
        for i in 0..200 {
            let degrees = 5.0 + (i % 40) as f64 * 3.0;
            let duration = 0.10 + 0.09 * (degrees / 1.5 + 1.0).log2();
            let counts = (degrees / 0.15) as i64;
            let samples = (duration / 0.008).round() as i64;
            for s in 0..=samples {
                let dx = counts / (samples + 1) + i64::from(s < counts % (samples + 1));
                csv.push_str(&format!("{},m,{dx},0\n", t + (s as f64 * duration / samples as f64 * 1e6) as u64));
            }
            t += (duration * 1e6) as u64 + 700_000;
        }
        csv.push_str(&format!("{t},f,0,0\n"));
        let path = std::env::temp_dir().join("rapidbot_recorder_test.csv");
        std::fs::write(&path, csv).unwrap();
        let report = run(path.to_str().unwrap(), 0.5).unwrap();
        let json = &report[report.find('{').unwrap()..];
        let profile: MouseProfile = serde_json::from_str(json).unwrap();
        assert!((profile.fitts_a - 0.10).abs() < 0.02, "a = {}", profile.fitts_a);
        assert!((profile.fitts_b - 0.09).abs() < 0.01, "b = {}", profile.fitts_b);
    }
}
