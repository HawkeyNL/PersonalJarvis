//! Trusted deployment-only validator for private Markdown agent profiles.
//! It never contacts GitHub, executes Markdown, or prints instructions.

use std::{env, fs, path::Path};

fn main() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args
        .next()
        .ok_or("usage: jarvis-agent-bundle validate INPUT OUTPUT")?;
    let input = args.next().ok_or("missing input")?;
    let output = args.next().ok_or("missing output")?;
    if command != "validate" || args.next().is_some() {
        return Err("usage: jarvis-agent-bundle validate INPUT OUTPUT".into());
    }
    let input = Path::new(&input);
    let output = Path::new(&output);
    let metadata = fs::symlink_metadata(input).map_err(|_| "agent source is unavailable")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() == 0 {
        return Err("agent source is unsafe or empty".into());
    }
    let source = fs::read_to_string(input).map_err(|_| "agent source is not UTF-8 text")?;
    let definition = jarvis_core::AgentLoader::parse_markdown(&source)
        .map_err(|_| "agent definition is invalid")?;
    let encoded = serde_json::to_vec(&definition).map_err(|_| "cannot encode agent definition")?;
    fs::write(output, encoded).map_err(|_| "cannot write validated definition")?;
    Ok(())
}
