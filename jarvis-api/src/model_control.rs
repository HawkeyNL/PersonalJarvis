//! Protected policy readback and serialization of signed model mutations.
//! Paths come from trusted startup configuration, never HTTP request data.

use jarvis_llm::ModelAccessPolicy;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};

pub struct ModelControl {
    path: Option<PathBuf>,
    unavailable_hf_routes: std::collections::BTreeSet<String>,
    pub(crate) mutation: tokio::sync::Mutex<()>,
}

impl ModelControl {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            unavailable_hf_routes: Default::default(),
            mutation: tokio::sync::Mutex::new(()),
        }
    }

    pub fn with_unavailable_hf_routes(mut self, models: impl IntoIterator<Item = String>) -> Self {
        self.unavailable_hf_routes.extend(models);
        self
    }

    pub(crate) fn can_enable(&self, provider: &str, model: &str) -> bool {
        provider != "huggingface" || !self.unavailable_hf_routes.contains(model)
    }

    pub(crate) fn read(&self) -> Result<(ModelAccessPolicy, String), &'static str> {
        let path = self.path.as_ref().ok_or("model control unavailable")?;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "model policy unavailable")?;
        let metadata = file.metadata().map_err(|_| "model policy unavailable")?;
        const LIMIT: u64 = 8 * 1024 * 1024;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.len() > LIMIT
        {
            return Err("unsafe model policy");
        }
        let mut raw = Vec::new();
        file.take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|_| "model policy unavailable")?;
        if raw.len() as u64 > LIMIT {
            return Err("model policy too large");
        }
        let policy: ModelAccessPolicy =
            serde_json::from_slice(&raw).map_err(|_| "invalid model policy")?;
        policy.validate().map_err(|_| "invalid model policy")?;
        Ok((policy, hex::encode(Sha256::digest(raw))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_control_has_no_caller_selected_file() {
        assert!(ModelControl::new(None).read().is_err());
    }
}
