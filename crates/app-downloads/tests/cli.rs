use std::io::Write;
use std::process::{Command, Output, Stdio};

fn run(input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_jarvis-app-downloads"))
        .arg("plan")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn native_cli_emits_a_plan_without_mutation_paths() {
    let result = run(br#"[{"target":"linux-x86_64","version":"1.0.0"}]"#);
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result[0]["keep"][0]["version"], "1.0.0");
    assert_eq!(result[0]["remove"], serde_json::json!([]));
}

#[test]
fn bad_input_does_not_echo_unknown_fields_or_produce_partial_plan() {
    for input in [
        br#"[{"target":"linux-x86_64","version":"1.0.0","path":"fixture-private-value"}]"#.as_slice(),
        br#"[{"target":"ios-arm64","version":"1.0.0"}]"#.as_slice(),
        br#"[{"target":"linux-x86_64","version":"1.0.0"},{"target":"linux-x86_64","version":"1.0.0"}]"#.as_slice(),
    ] {
        let result = run(input);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("fixture-private-value"));
    }
}

#[test]
fn oversized_input_is_rejected_before_deserialization() {
    let result = run(&vec![b' '; 2 * 1024 * 1024 + 1]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("2 MiB"));
}
