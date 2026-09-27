//! Credential-free fixed workload entrypoint. The production image installs
//! this exact executable at /usr/local/bin/jarvis-codex-runtime.

use std::{env, path::Path};

fn main() {
    let arguments: Vec<String> = env::args().collect();
    if arguments.len() != 3
        || arguments[1] != "run-approved-task"
        || arguments[2] != "/workspace/input/request.json"
    {
        eprintln!("invalid Codex workload invocation");
        std::process::exit(2);
    }
    if jarvis_codex::runtime::run_in_workspace(Path::new("/workspace")).is_err() {
        eprintln!("Codex workload rejected or unavailable");
        std::process::exit(1);
    }
}
