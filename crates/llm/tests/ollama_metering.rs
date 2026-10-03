//! Whether `ollama` is metered depends on the configured URL, a process-wide
//! fact, so this runs in its own test binary.

use jarvis_llm::{
    build_provider, is_metered_backend, HuggingFaceBackend, OpenAiBackend, ProviderConfig,
    RoutingSnapshot,
};

fn build_with_ollama(url: &str) {
    build_provider(ProviderConfig {
        provider: "ollama".into(),
        api_key: None,
        anthropic_base_url: "https://api.anthropic.com".into(),
        model_default: "claude-sonnet-5".into(),
        model_hard: "claude-opus-5".into(),
        model_cheap: "claude-haiku-4-5".into(),
        ollama_url: url.into(),
        ollama_model: "llama3.2".into(),
        claude_cli_bin: "claude".into(),
        openai: OpenAiBackend::default(),
        deepseek: OpenAiBackend::default(),
        xai: OpenAiBackend::default(),
        zai: OpenAiBackend::default(),
        ollama_cloud: OpenAiBackend::default(),
        huggingface: HuggingFaceBackend::default(),
    });
}

#[test]
fn remote_ollama_is_metered_and_refused_with_paid_api_off() {
    let paid_off = RoutingSnapshot::unavailable("routing_invalid");

    build_with_ollama("http://192.168.1.20:11434");
    assert!(is_metered_backend("ollama"));
    assert!(paid_off.refuses_provider("ollama"));

    build_with_ollama("http://127.0.0.1:11434");
    assert!(!is_metered_backend("ollama"));
    assert!(!paid_off.refuses_provider("ollama"));
}
