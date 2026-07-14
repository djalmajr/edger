use std::io::{self, BufRead, Write};

use edger_mcp::discovery::McpContext;

fn main() -> anyhow::Result<()> {
    let ctx = McpContext::from_env()?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = edger_mcp::handle_line(&ctx, &line);
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }

    Ok(())
}
