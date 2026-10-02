//! Records your own input while you play Minecraft, and fits the human
//! models to it.
//!
//!     rapidbot-recorder record session1.csv     # play; Ctrl+C to stop
//!     rapidbot-recorder analyze session1.csv [sensitivity] > profile.json
//!
//! What is recorded, and only while a window titled "Minecraft..." is in
//! front: raw mouse movement (the same counts the game gets), mouse
//! buttons, and a fixed list of game keys (W A S D, space, shift, ctrl,
//! 1-9, E, Q, F). Other keys are ignored, so chat and passwords are never
//! captured. Each line is `microseconds,kind,a,b`:
//! `m,dx,dy` movement, `b,button,down`, `k,name,down`, `f,focused,0`.

mod analyze;
#[cfg(windows)]
mod record;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        #[cfg(windows)]
        Some("record") => {
            let path = args.get(1).cloned().unwrap_or_else(|| "recording.csv".into());
            if let Err(e) = record::run(&path) {
                eprintln!("recording failed: {e}");
                std::process::exit(1);
            }
        }
        Some("analyze") => {
            let Some(path) = args.get(1) else {
                eprintln!("usage: rapidbot-recorder analyze <file.csv> [sensitivity 0-1, default 0.5]");
                std::process::exit(2);
            };
            let sensitivity = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.5);
            match analyze::run(path, sensitivity) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("analysis failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("usage:\n  rapidbot-recorder record <file.csv>\n  rapidbot-recorder analyze <file.csv> [sensitivity]");
            std::process::exit(2);
        }
    }
}
