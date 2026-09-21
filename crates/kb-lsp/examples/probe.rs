//! Talks to a real server: `cargo run -p kb-lsp --example probe -- <root> <file> <line> <col>`.
//! Prints what comes back. The protocol layer's sanity check, the way
//! `kb-term`'s `dump` is the terminal's.

use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&args[1]);
    let file = PathBuf::from(&args[2]);
    let pos = kb_lsp::Pos { line: args[3].parse().unwrap(), col: args[4].parse().unwrap() };
    let mut client = kb_lsp::Client::spawn(&kb_lsp::Options {
        command: std::env::var("LSP").unwrap_or_else(|_| "rust-analyzer".into()),
        args: Vec::new(),
        root,
        language: "rust".into(),
    })
    .expect("spawn");
    let text = std::fs::read_to_string(&file).unwrap();
    client.sync(&file, 1, || text.clone());

    let started = Instant::now();
    let mut asked = false;
    while started.elapsed() < Duration::from_secs(40) {
        for event in client.poll() {
            println!("{:>5} ms  {event:?}", started.elapsed().as_millis());
        }
        // Ask once the server has had time to load the crate graph.
        if !asked && client.is_ready() && started.elapsed() > Duration::from_secs(15) {
            asked = true;
            client.hover(&file, pos);
            client.definition(&file, pos);
            client.completion(&file, pos);
            client.format(&file, 4);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
