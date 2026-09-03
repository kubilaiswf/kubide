//! One turn against the real CLI, events printed as they land.
//!
//! The protocol is checked by the unit tests against recorded lines; this
//! is the check against the live thing, the way `kb-term --example dump`
//! and `kb-git --example status` are for their layers. It spends one
//! short turn of whatever the CLI is logged in as.
//!
//!     cargo run -p kb-agent --example chat -- "say hello in five words"

use std::time::{Duration, Instant};

use kb_agent::{Agent, Event, Options};

fn main() {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Reply with the single word: pong".to_string());
    let opts = Options {
        command: "claude".into(),
        cwd: std::env::current_dir().expect("cwd"),
        model: None,
        permission_mode: "plan".into(),
        allowed_tools: Vec::new(),
        resume: None,
    };
    let mut agent = match Agent::spawn(&opts) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("spawn failed: {e}");
            std::process::exit(1);
        }
    };
    agent.send(&prompt).expect("send");

    let started = Instant::now();
    let deadline = Duration::from_secs(120);
    loop {
        for event in agent.poll() {
            match &event {
                Event::TextDelta { text, .. } => print!("{text}"),
                other => println!("\n{other:?}"),
            }
            if matches!(event, Event::Result { .. }) {
                println!("\n--- turn done in {:.1}s", started.elapsed().as_secs_f64());
                return;
            }
            if matches!(event, Event::Exited(_)) {
                println!("\n--- exited before a result");
                std::process::exit(2);
            }
        }
        if started.elapsed() > deadline {
            println!("\n--- gave up after {}s", deadline.as_secs());
            std::process::exit(3);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
