//! `luna-terminal` — terminal Luny.

use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    luna_terminal::run()
}
