//! Credential entry remains in the trusted TTY helper; no secret enters Rust.
use super::*;

fn model_provider(provider: &CredentialProvider) -> Option<Provider> {
    match provider {
        CredentialProvider::Anthropic => Some(Provider::AnthropicApi),
        CredentialProvider::Openai => Some(Provider::OpenaiApi),
        CredentialProvider::Deepseek => Some(Provider::DeepseekApi),
        CredentialProvider::Xai => Some(Provider::XaiApi),
        CredentialProvider::Zai => Some(Provider::ZaiApi),
        CredentialProvider::OllamaCloud => Some(Provider::OllamaCloud),
        CredentialProvider::Huggingface => Some(Provider::Huggingface),
        CredentialProvider::Jev => None,
    }
}

pub(super) fn set(provider: &CredentialProvider, verbose: bool) -> Result<()> {
    setup(provider, |helper, args| {
        compatibility_helper(helper, args, verbose)
    })
}

fn setup(
    provider: &CredentialProvider,
    mut run: impl FnMut(AdminHelper, Vec<String>) -> Result<()>,
) -> Result<()> {
    run(
        AdminHelper::Credentials,
        vec!["set".into(), provider.as_str().into()],
    )?;
    let Some(provider) = model_provider(provider) else {
        return Ok(());
    };
    run(AdminHelper::Models, vec!["refresh".into(), provider.as_str().into()])
        .with_context(|| format!(
            "credential verified and saved, but model catalog refresh failed; retry: sudo jarvis models refresh {}",
            provider.as_str()
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_credential_maps_to_its_own_catalog_only() {
        for (provider, model_provider) in [
            (CredentialProvider::Anthropic, "anthropic-api"),
            (CredentialProvider::Openai, "openai-api"),
            (CredentialProvider::Deepseek, "deepseek-api"),
            (CredentialProvider::Xai, "xai-api"),
            (CredentialProvider::Zai, "zai-api"),
            (CredentialProvider::OllamaCloud, "ollama-cloud"),
            (CredentialProvider::Huggingface, "huggingface"),
        ] {
            let mut calls = Vec::new();
            setup(&provider, |helper, args| {
                calls.push((helper, args));
                Ok(())
            })
            .unwrap();
            assert_eq!(
                calls,
                vec![
                    (
                        AdminHelper::Credentials,
                        vec!["set".into(), provider.as_str().into()]
                    ),
                    (
                        AdminHelper::Models,
                        vec!["refresh".into(), model_provider.into()]
                    ),
                ]
            );
        }
    }

    #[test]
    fn rejected_credential_never_triggers_discovery() {
        let mut calls = 0;
        assert!(setup(&CredentialProvider::Openai, |helper, _| {
            calls += 1;
            assert_eq!(helper, AdminHelper::Credentials);
            bail!("probe refused")
        })
        .is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn jev_credential_does_not_invoke_chat_model_discovery() {
        let mut calls = Vec::new();
        setup(&CredentialProvider::Jev, |helper, args| {
            calls.push((helper, args));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![(AdminHelper::Credentials, vec!["set".into(), "jev".into()])]
        );
    }

    #[test]
    fn discovery_failure_is_reported_as_partial_success() {
        let error = setup(&CredentialProvider::Openai, |helper, _| {
            if helper == AdminHelper::Models {
                bail!("offline")
            }
            Ok(())
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("verified and saved"));
        assert!(error.contains("sudo jarvis models refresh openai-api"));
    }
}
