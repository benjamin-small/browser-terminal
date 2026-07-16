//! Native REPL over bterm-core — the everyday dev harness.
//! Milestone 1: echoes lines back through the core placeholder.

use std::io::{self, BufRead, Write};

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    write!(stdout, "bterm> ")?;
    stdout.flush()?;
    for line in stdin.lock().lines() {
        let line = line?;
        writeln!(stdout, "{}", bterm_core::echo_transform(&line))?;
        write!(stdout, "bterm> ")?;
        stdout.flush()?;
    }
    Ok(())
}
