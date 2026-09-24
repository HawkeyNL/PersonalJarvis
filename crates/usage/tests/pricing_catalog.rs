use jarvis_usage::{
    cost_eur_with_registry, estimate_task_cost_with_registry, PriceStatus, PricingRegistry,
};

#[test]
fn legacy_defaults_update_custom_overrides_survive_and_rollback_is_read_only() {
    for raw in [
        include_str!("../data/legacy-pricing-2026-08-27.json"),
        include_str!("../data/legacy-pricing-2026-09-01.json"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner.json");
        std::fs::write(&path, raw).unwrap();
        let current = PricingRegistry::load_with_builtin(&path).unwrap();
        assert_eq!(current.price_for("openai-api", "gpt-4.1").0.input, 2.0);
        assert_eq!(current.price_for("openai-api", "gpt-6-astra").0.input, 10.0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        let historical = PricingRegistry::load(&path).unwrap();
        assert_eq!(historical.price_for("openai-api", "gpt-4.1").0.input, 2.5);
        let mut owner = historical;
        let custom = owner
            .models
            .iter_mut()
            .find(|e| e.model == "gpt-4.1")
            .unwrap();
        custom.input_per_million_usd = 7.0;
        owner
            .models
            .iter_mut()
            .find(|e| e.model == "gpt-4o")
            .unwrap()
            .owner_override = true;
        let upgraded = owner.with_release(PricingRegistry::builtin()).unwrap();
        assert_eq!(upgraded.price_for("openai-api", "gpt-4.1").0.input, 7.0);
        let kept = upgraded
            .models
            .iter()
            .find(|e| e.model == "gpt-4o")
            .unwrap();
        assert!(kept.owner_override);
        assert_eq!(kept.cache_read_per_million_usd, None);
    }
}

#[test]
fn empty_owner_registry_tracks_future_release_prices_without_copying_defaults() {
    let raw = include_str!("../../../deploy/systemd/pricing-overrides.empty.json");
    let mut next = PricingRegistry::builtin();
    next.models
        .iter_mut()
        .find(|e| e.model == "gpt-6-astra")
        .unwrap()
        .input_per_million_usd = 11.0;
    let effective = serde_json::from_str::<PricingRegistry>(raw)
        .unwrap()
        .with_release(next)
        .unwrap();
    assert_eq!(
        effective.price_for("openai-api", "gpt-6-astra").0.input,
        11.0
    );
}

#[test]
fn packaged_catalog_and_runtime_are_identical_and_valid() {
    let packaged: serde_json::Value = serde_json::from_str(include_str!(
        "../../../deploy/systemd/pricing-registry.json"
    ))
    .unwrap();
    let runtime = PricingRegistry::builtin();
    runtime.validate().unwrap();
    let decoded: PricingRegistry = serde_json::from_value(packaged).unwrap();
    assert_eq!(
        serde_json::to_value(runtime).unwrap(),
        serde_json::to_value(decoded).unwrap()
    );
}

#[test]
fn reviewed_providers_have_exact_prices_and_provenance() {
    let registry = PricingRegistry::builtin();
    for (provider, model, input, cache, output) in [
        ("openai-api", "gpt-6-astra", 10.0, 1.0, 50.0),
        ("openai-api", "gpt-6-sol", 2.0, 0.2, 10.0),
        ("openai-api", "gpt-6-luna", 0.1, 0.01, 0.5),
        ("anthropic-api", "claude-sonnet-5", 2.0, 0.2, 10.0),
        ("deepseek-api", "deepseek-flash", 0.3, 0.006, 1.2),
        ("xai-api", "grok-4.7", 2.0, 0.5, 6.0),
        ("zai-api", "glm-5.3", 1.4, 0.26, 4.4),
        ("ollama-cloud", "gpt-oss:20b", 0.07, 0.035, 0.3),
    ] {
        let (price, status) = registry.price_for(provider, model);
        assert_ne!(status, PriceStatus::Unknown, "{provider}/{model}");
        assert_eq!(
            (price.input, price.cache_read, price.output),
            (input, cache, output)
        );
        let entry = registry
            .models
            .iter()
            .find(|e| e.provider == provider && e.model == model)
            .unwrap();
        assert!(entry
            .pricing_source
            .as_ref()
            .unwrap()
            .starts_with("https://"));
        assert_eq!(entry.pricing_updated_at.as_deref(), Some("2026-09-24"));
        assert_eq!(
            registry
                .price_for(provider, &format!("{model}-unreviewed"))
                .1,
            PriceStatus::Unknown
        );
    }
    assert_eq!(
        registry.price_for("huggingface", "org/new-model").1,
        PriceStatus::Unknown
    );
    assert!(registry.price_for("huggingface", "org/new-model").0.input > 0.0);
}

#[test]
fn long_context_includes_cached_tokens_and_uses_whole_request_rates() {
    let registry = PricingRegistry::builtin();
    let short = cost_eur_with_registry(
        &registry,
        "openai-api",
        "gpt-6-astra",
        272_000,
        1_000,
        0,
        1.0,
    );
    let long = cost_eur_with_registry(
        &registry,
        "openai-api",
        "gpt-6-astra",
        272_000,
        1_000,
        1,
        1.0,
    );
    assert!((short - 2.77).abs() < 1e-9);
    assert!((long - 5.515002).abs() < 1e-9);
    let grok = cost_eur_with_registry(&registry, "xai-api", "grok-4.7", 200_000, 1_000, 0, 1.0);
    assert!((grok - 0.812).abs() < 1e-9);
    let per_call = estimate_task_cost_with_registry(
        &registry,
        "openai-api",
        "gpt-6-astra",
        200_000,
        1_000,
        2,
        1.0,
    );
    assert!((per_call.likely_eur - 4.1).abs() < 1e-9);
}

#[test]
fn unknown_cache_discount_is_not_invented() {
    let registry: PricingRegistry = serde_json::from_str(r#"{"version":1,"source":"owner","updated_at":"2026-09-24","models":[{"provider":"openai-api","model":"custom","input_per_million_usd":2,"output_per_million_usd":8}]}"#).unwrap();
    assert_eq!(registry.price_for("openai-api", "custom").0.cache_read, 2.0);
}

#[test]
fn invalid_long_context_rates_are_rejected() {
    let mut registry = PricingRegistry::builtin();
    let entry = registry
        .models
        .iter_mut()
        .find(|e| e.model == "gpt-6-astra")
        .unwrap();
    entry.long_context.as_mut().unwrap().input_per_million_usd = -1.0;
    assert!(registry.validate().is_err());
}
