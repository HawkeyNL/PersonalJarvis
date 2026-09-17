//! Fixed-path, root-provisioned, short-lived first-device activation policy.
use super::BootstrapEnrollment;
use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

pub const ACTIVATION_FILE: &str = "/etc/jarvis/account-activation.json";

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationPolicy {
    pub schema_version: u8,
    pub secret_sha256: String,
    pub allowed_cidrs: Vec<String>,
    pub issued_at: u64,
    pub expires_at: u64,
}

impl ActivationPolicy {
    pub fn validate(&self, now: u64) -> Result<BootstrapEnrollment, &'static str> {
        if self.schema_version != 1
            || self.issued_at > now
            || self.expires_at <= now
            || self.expires_at.saturating_sub(self.issued_at) > 600
            || self.allowed_cidrs.is_empty()
            || self.allowed_cidrs.len() > 8
        {
            return Err("activation code expired or invalid");
        }
        let hash = hex::decode(&self.secret_sha256).map_err(|_| "invalid activation verifier")?;
        let secret_hash = hash.try_into().map_err(|_| "invalid activation verifier")?;
        let allowed_cidrs = self
            .allowed_cidrs
            .iter()
            .map(|cidr| cidr.parse())
            .collect::<Result<Vec<ipnet::IpNet>, _>>()
            .map_err(|_| "invalid activation network")?;
        // Activation remains restricted to an explicit loopback/private LAN.
        for network in &allowed_cidrs {
            let safe = |ip: std::net::IpAddr| match ip {
                std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
                std::net::IpAddr::V6(ip) => ip.octets()[0] & 0xfe == 0xfc || ip.is_loopback(),
            };
            if !safe(network.network()) || !safe(network.broadcast()) {
                return Err("activation requires a private LAN or loopback range");
            }
        }
        Ok(BootstrapEnrollment {
            secret_hash,
            allowed_cidrs,
            expires_at: Some(self.expires_at),
        })
    }
}

pub fn load() -> Result<Option<BootstrapEnrollment>, &'static str> {
    for path in ["/etc", "/etc/jarvis"] {
        let info = fs::symlink_metadata(path).map_err(|_| "activation unavailable")?;
        if !info.is_dir() || info.uid() != 0 || info.mode() & 0o022 != 0 {
            return Err("unsafe activation directory");
        }
    }
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(Path::new(ACTIVATION_FILE))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("activation unavailable"),
    };
    let info = file.metadata().map_err(|_| "activation unavailable")?;
    if !info.is_file()
        || info.uid() != 0
        || info.mode() & 0o027 != 0
        || info.nlink() != 1
        || info.len() > 4096
    {
        return Err("unsafe activation policy");
    }
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| "activation unavailable")?;
    if bytes.len() > 4096 {
        return Err("activation policy too large");
    }
    let policy: ActivationPolicy =
        serde_json::from_slice(&bytes).map_err(|_| "invalid activation policy")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "activation clock unavailable")?
        .as_secs();
    policy.validate(now).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_has_strict_expiry_and_private_network_scope() {
        let mut policy = ActivationPolicy {
            schema_version: 1,
            secret_sha256: "ab".repeat(32),
            allowed_cidrs: vec!["10.23.45.0/24".into()],
            issued_at: 1000,
            expires_at: 1600,
        };
        let parsed = policy.validate(1001).unwrap();
        assert!(parsed.allows("10.23.45.10".parse().unwrap()));
        assert!(!parsed.allows("192.0.2.10".parse().unwrap()));
        assert!(policy.validate(1600).is_err());
        assert!(policy.validate(999).is_err());
        policy.expires_at = 1601;
        assert!(policy.validate(1001).is_err());
        policy.expires_at = 1600;
        policy.allowed_cidrs = vec!["0.0.0.0/0".into()];
        assert!(policy.validate(1001).is_err());
    }
}
