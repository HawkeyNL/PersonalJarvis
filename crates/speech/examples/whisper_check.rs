// Diagnostic: try to load a Whisper GGML model and report the exact failure.
// Run: cargo run -p jarvis-speech --example whisper_check --features whisper -- models/ggml-base.bin
#[cfg(feature = "whisper")]
fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "models/ggml-base.bin".to_string());
    eprintln!("[check] loading: {path}");
    match whisper_rs::WhisperContext::new_with_params(
        &path,
        whisper_rs::WhisperContextParameters::default(),
    ) {
        Ok(_) => eprintln!("[check] OK — context created"),
        Err(e) => eprintln!("[check] FAIL — {e:?}"),
    }
}

#[cfg(not(feature = "whisper"))]
fn main() {
    eprintln!("build with --features whisper");
}
