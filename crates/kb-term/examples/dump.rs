//! ConPTY check, without opening a window.
//!
//! Starts a shell, runs a command, dumps the grid as text. Proves the terminal
//! layer works independently of rendering — the first place to look when
//! something breaks and you need to know whether it's the PTY or the drawing.
//!
//!     cargo run -p kb-term --example dump

use std::time::{Duration, Instant};

use kb_term::SpawnOptions;

fn main() {
    let (cols, rows) = (100, 24);
    println!("starting shell ({cols}x{rows})...");

    let opts = SpawnOptions {
        cols,
        rows,
        ..Default::default()
    };
    let term = match kb_term::Terminal::spawn(&opts) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    };

    // Give the shell time to print a prompt.
    std::thread::sleep(Duration::from_millis(1200));
    term.write(b"echo kubide-conpty-ok\r\n");

    // Poll for the expected string rather than sleeping a fixed amount, so the
    // result is the same on a slow machine and a fast one.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut snap = term.snapshot();
    let mut found = false;
    while Instant::now() < deadline {
        snap = term.snapshot();
        if grid_text(&snap).contains("kubide-conpty-ok") {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    println!("\n--- grid {}x{} ---", snap.cols, snap.rows);
    for line in grid_text(&snap).lines() {
        if !line.trim().is_empty() {
            println!("{line}");
        }
    }
    println!("--- cursor: col {} row {} ---", snap.cursor.0, snap.cursor.1);

    // The line we typed is the command itself; the proof is its OUTPUT, so the
    // string has to appear at least twice.
    let hits = grid_text(&snap).matches("kubide-conpty-ok").count();
    println!();
    if !found || hits < 2 {
        println!("RESULT: FAILED — command output never reached the grid (hits={hits}).");
        std::process::exit(1);
    }
    println!("[1/2] ConPTY: command ran and its output landed in the grid.");

    // --- Scrollback ---
    //
    // Regression test for a bug you can't see in code: `grid[Line(n)]` ignores
    // display_offset, so unless the snapshot shifts by hand the wheel appears
    // to do nothing. A compiler will never catch this.
    term.write(b"1..120 | ForEach-Object { \"line-$_\" }\r\n");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if grid_text(&term.snapshot()).contains("line-120") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let bottom = term.snapshot();
    if bottom.scrolled_back {
        println!("RESULT: FAILED — scrolled_back is true before scrolling.");
        std::process::exit(1);
    }

    term.scroll(40);
    let scrolled = term.snapshot();

    let bottom_text = grid_text(&bottom);
    let scrolled_text = grid_text(&scrolled);

    println!("\n--- first non-empty line of the scrolled view ---");
    if let Some(l) = scrolled_text.lines().find(|l| !l.trim().is_empty()) {
        println!("{l}");
    }

    if !scrolled.scrolled_back {
        println!("\nRESULT: FAILED — scrolled_back is false after scroll().");
        std::process::exit(1);
    }
    if scrolled_text == bottom_text {
        println!("\nRESULT: FAILED — content unchanged after scrolling.");
        println!("        (display_offset may not be applied in the snapshot)");
        std::process::exit(1);
    }

    // Typing must jump back to the bottom.
    term.write(b"\r\n");
    std::thread::sleep(Duration::from_millis(400));
    if term.snapshot().scrolled_back {
        println!("\nRESULT: FAILED — did not return to the bottom after typing.");
        std::process::exit(1);
    }

    println!("[2/2] Scrollback: scrolling changed the view, typing returned to the bottom.");
    println!("\nRESULT: terminal layer is sound.");
}

fn grid_text(s: &kb_term::Snapshot) -> String {
    let mut out = String::with_capacity(s.cols * s.rows + s.rows);
    for row in 0..s.rows {
        for col in 0..s.cols {
            out.push(s.cell(col, row).map(|c| c.ch).unwrap_or(' '));
        }
        out.push('\n');
    }
    out
}
