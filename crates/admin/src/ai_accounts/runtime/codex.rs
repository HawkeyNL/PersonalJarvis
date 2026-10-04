//! Owner-triggered install of the official Codex CLI runtime.
//!
//! OpenAI signs each release binary keylessly with Sigstore from its GitHub
//! release workflow. Integrity comes from `cosign verify-blob` against that
//! exact workflow identity and the Rekor entry in the published bundle, plus
//! GitHub's SHA-256 digests of the downloaded assets. The candidate is never
//! executed here: status runs it only as `jarvis-codex` in the hardened
//! transient service, and the chat worker gates the reviewed version.
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Serialize;
use serde_json::Value;

use super::{
    check_destination, copy_hashed, curl, installed_matches, installed_version, open_no_follow,
    owned_regular_file, replace_target, strict_version, work_directory, Channel, Layout,
    VersionRequest, BINARY_LIMIT,
};
use crate::ai_accounts::{
    resolve_trusted_executable, validate_root_executable, validate_system_executable,
    AccountProvider, SYSTEM_LINK_HOPS,
};

const GZIP: &str = "/usr/bin/gzip";
/// Exact signer: OpenAI's release workflow at the requested tag.
const IDENTITY_PREFIX: &str =
    "https://github.com/openai/codex/.github/workflows/rust-release.yml@refs/tags/rust-v";
const ISSUER: &str = "https://token.actions.githubusercontent.com";
const RELEASE_LIMIT: u64 = 4 * 1024 * 1024;
const BUNDLE_LIMIT: u64 = 64 * 1024;
/// End-of-archive marker plus record fill after the single member.
const TRAILER_LIMIT: u64 = 1024 * 1024;
const COSIGN_TIMEOUT: Duration = Duration::from_secs(180);

/// Fixed release source. Only tests construct another one.
pub(super) struct Source<'a> {
    releases: &'a str,
    downloads: &'a str,
    /// curl `--proto` value; production accepts HTTPS only.
    protocol: &'a str,
    /// The only URL prefixes GitHub's download redirect may point to.
    redirects: &'a [&'a str],
    /// Absolute path below the layout's trusted root.
    cosign: &'a str,
}

pub(super) const SOURCE: Source<'static> = Source {
    releases: "https://api.github.com/repos/openai/codex/releases",
    downloads: "https://github.com/openai/codex/releases/download",
    protocol: "=https",
    redirects: &[
        "https://objects.githubusercontent.com/",
        "https://release-assets.githubusercontent.com/",
    ],
    cosign: "/usr/bin/cosign",
};

pub(super) const LAYOUT: Layout<'static> = Layout {
    target: "/usr/local/bin/codex",
    previous: "/usr/local/lib/jarvis/codex.previous",
    trusted_root: "/",
    owner: (0, 0),
};

#[derive(Debug, Serialize)]
struct Status {
    provider: &'static str,
    installed: bool,
    version: Option<String>,
    safe_ownership: bool,
    latest: Option<String>,
    update_available: bool,
    rollback_available: bool,
    cosign_available: bool,
}

#[derive(Debug, PartialEq)]
struct Asset {
    digest: String,
    size: u64,
}

pub(super) fn print_status(json: bool) -> Result<()> {
    let status = status();
    if json {
        println!("{}", serde_json::to_string(&status)?);
    } else {
        println!(
            "codex runtime: installed={} version={} safe_ownership={} latest={} update_available={} rollback_available={} cosign_available={}",
            status.installed,
            status.version.as_deref().unwrap_or("unknown"),
            status.safe_ownership,
            status.latest.as_deref().unwrap_or("unknown"),
            status.update_available,
            status.rollback_available,
            status.cosign_available
        );
    }
    Ok(())
}

fn status() -> Status {
    let installed = fs::symlink_metadata(LAYOUT.target).is_ok();
    let safe_ownership = validate_root_executable(LAYOUT.target).is_ok();
    let version = safe_ownership
        .then(|| installed_version(AccountProvider::Codex))
        .flatten();
    let latest = tempfile::tempdir().ok().and_then(|dir| {
        let release = dir.path().join("release.json");
        let url = format!("{}/latest", SOURCE.releases);
        let (code, _) = curl(SOURCE.protocol, &url, &release, RELEASE_LIMIT, 10).ok()?;
        (code == "200").then_some(())?;
        release_version(&serde_json::from_slice(&fs::read(release).ok()?).ok()?).ok()
    });
    let update_available = latest.as_deref().is_some_and(|latest| {
        version
            .as_deref()
            .and_then(strict_version)
            .is_none_or(|installed| strict_version(latest) > Some(installed))
    });
    Status {
        provider: "codex",
        installed,
        version,
        safe_ownership,
        latest,
        update_available,
        rollback_available: owned_regular_file(&LAYOUT, Path::new(LAYOUT.previous)).is_ok(),
        cosign_available: cosign(&SOURCE, &LAYOUT).is_ok(),
    }
}

/// The musl builds are static and run on glibc and musl hosts alike.
pub(super) fn target() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" => Ok("aarch64-unknown-linux-musl"),
        _ => bail!("this architecture has no official Codex runtime"),
    }
}

pub(super) fn install(
    layout: &Layout,
    source: &Source,
    request: &VersionRequest,
    target: &str,
) -> Result<String> {
    let release_path = match request {
        VersionRequest::Exact(version) if strict_version(version).is_some() => {
            format!("tags/rust-v{version}")
        }
        VersionRequest::Exact(_) => {
            bail!("runtime version must be a strict MAJOR.MINOR.PATCH version")
        }
        VersionRequest::Channel(Channel::Latest) => "latest".to_owned(),
        VersionRequest::Channel(Channel::Stable) => {
            bail!("Codex has no stable channel; use --channel latest or --version")
        }
    };
    check_destination(layout)?;
    let cosign = cosign(source, layout)?;
    let work = work_directory(layout)?;
    let release = work.path().join("release.json");
    let url = format!("{}/{release_path}", source.releases);
    let (code, _) = curl(source.protocol, &url, &release, RELEASE_LIMIT, 30)
        .context("Codex release metadata download failed")?;
    if code != "200" {
        bail!("Codex release metadata download failed");
    }
    let release: Value =
        serde_json::from_slice(&fs::read(&release)?).context("parse Codex release metadata")?;
    let version = release_version(&release)?;
    if let VersionRequest::Exact(requested) = request {
        if &version != requested {
            bail!("GitHub returned metadata for a different Codex release");
        }
    }
    let member = format!("codex-{target}");
    let bundle_name = format!("{member}.sigstore");
    let archive_name = format!("{member}.tar.gz");
    let bundle_asset = asset(&release, &bundle_name, BUNDLE_LIMIT)?;
    let archive_asset = asset(&release, &archive_name, BINARY_LIMIT)?;

    let bundle = work.path().join("codex.sigstore");
    download(source, &version, &bundle_name, &bundle, &bundle_asset, 60)?;
    let signed = rekor_digest(&fs::read(&bundle)?)?;
    let archive = work.path().join("codex.tar.gz");
    download(
        source,
        &version,
        &archive_name,
        &archive,
        &archive_asset,
        900,
    )?;
    let binary = work.path().join("codex");
    let (digest, size) = extract(&archive, &member, &binary)?;
    // Defence in depth: cosign checks this too.
    if digest != signed {
        bail!("extracted Codex binary does not match the SHA-256 in the signed Rekor entry");
    }
    verify_blob(&cosign, work.path(), &bundle, &binary, &version)?;
    // Reinstalling the active version must not replace the rollback copy.
    if installed_matches(layout, &digest, size)? {
        return Ok(version);
    }
    check_destination(layout)?;
    replace_target(layout, &binary, Some((&digest, size)))?;
    Ok(version)
}

/// `rust-vMAJOR.MINOR.PATCH` is the only accepted release tag.
fn release_version(release: &Value) -> Result<String> {
    release
        .get("tag_name")
        .and_then(Value::as_str)
        .and_then(|tag| tag.strip_prefix("rust-v"))
        .filter(|version| strict_version(version).is_some())
        .map(str::to_owned)
        .context("Codex release tag is not rust-vMAJOR.MINOR.PATCH")
}

/// The single asset named `name` with GitHub's SHA-256 digest and its size.
fn asset(release: &Value, name: &str, limit: u64) -> Result<Asset> {
    let mut matches = release
        .get("assets")
        .and_then(Value::as_array)
        .context("Codex release metadata has no assets")?
        .iter()
        .filter(|asset| asset.get("name").and_then(Value::as_str) == Some(name));
    let (Some(asset), None) = (matches.next(), matches.next()) else {
        bail!("Codex release must have exactly one {name}");
    };
    let digest = asset
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| sha256_hex(digest))
        .context("Codex release asset has no valid SHA-256 digest")?;
    let size = asset
        .get("size")
        .and_then(Value::as_u64)
        .filter(|size| (1..=limit).contains(size))
        .context("Codex release asset size is missing or exceeds its bound")?;
    Ok(Asset {
        digest: digest.to_owned(),
        size,
    })
}

/// GitHub answers a release download with one redirect to its asset storage.
/// That single hop is followed here, and only to an allowed HTTPS prefix.
fn download(
    source: &Source,
    version: &str,
    name: &str,
    output: &Path,
    asset: &Asset,
    seconds: u32,
) -> Result<()> {
    let url = format!("{}/rust-v{version}/{name}", source.downloads);
    let (code, location) = curl(source.protocol, &url, output, asset.size, 30)
        .with_context(|| format!("runtime download failed: {name}"))?;
    if !matches!(code.as_str(), "301" | "302" | "303" | "307" | "308") {
        bail!("{name} download did not redirect to GitHub asset storage");
    }
    let location = allowed_redirect(source, &location)?;
    let (code, _) = curl(source.protocol, location, output, asset.size, seconds)
        .with_context(|| format!("runtime download failed: {name}"))?;
    if code != "200" {
        bail!("runtime download failed: {name}");
    }
    let (digest, length) = copy_hashed(&mut open_no_follow(output)?, &mut io::sink())?;
    if length != asset.size || digest != asset.digest {
        bail!("{name} does not match GitHub's published SHA-256 digest");
    }
    Ok(())
}

fn allowed_redirect<'a>(source: &Source, location: &'a str) -> Result<&'a str> {
    let allowed = source
        .redirects
        .iter()
        .any(|prefix| location.starts_with(prefix))
        && location.bytes().all(|byte| byte.is_ascii_graphic());
    if !allowed {
        bail!("release download redirected outside GitHub asset storage");
    }
    Ok(location)
}

/// The SHA-256 that the bundle's Rekor `hashedrekord` entry records for the
/// signed binary.
fn rekor_digest(bytes: &[u8]) -> Result<String> {
    let bundle: Value = serde_json::from_slice(bytes).context("parse Codex Sigstore bundle")?;
    let body = bundle
        .pointer("/rekorBundle/Payload/body")
        .and_then(Value::as_str)
        .context("Codex Sigstore bundle has no Rekor entry")?;
    let entry: Value = serde_json::from_slice(&BASE64.decode(body).context("decode Rekor entry")?)
        .context("parse Rekor entry")?;
    let hash = entry.pointer("/spec/data/hash");
    if entry.get("kind").and_then(Value::as_str) != Some("hashedrekord")
        || hash
            .and_then(|hash| hash.get("algorithm"))
            .and_then(Value::as_str)
            != Some("sha256")
    {
        bail!("Rekor entry is not a SHA-256 hashedrekord");
    }
    hash.and_then(|hash| hash.get("value"))
        .and_then(Value::as_str)
        .filter(|value| sha256_hex(value))
        .map(str::to_owned)
        .context("Rekor entry has no valid SHA-256")
}

fn sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Decompresses with the host's gzip and keeps only the single expected
/// member. Input is bounded by the verified archive size and output by the
/// member bound, so no separate time limit is needed.
fn extract(archive: &Path, member: &str, output: &Path) -> Result<(String, u64)> {
    validate_system_executable(GZIP)?;
    let mut child = Command::new(GZIP)
        .args(["--decompress", "--stdout", "--"])
        .arg(archive)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start Codex archive decompression")?;
    let stream = child
        .stdout
        .take()
        .context("missing decompressed archive")?;
    let copied = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .context("create extracted runtime")
        .and_then(|mut file| untar(stream, member, &mut file));
    if copied.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let copied = copied?;
    if !status.success() {
        bail!("Codex archive is not a valid gzip stream");
    }
    Ok(copied)
}

/// Accepts exactly one ustar/GNU header for the regular file `member`
/// followed by zero padding and the end-of-archive marker. Links, other
/// paths (absolute, `..`, prefixed), extension headers and extra members are
/// refused; GitHub's digest covers the archive, not what is inside it.
fn untar(mut reader: impl Read, member: &str, output: &mut impl Write) -> Result<(String, u64)> {
    let mut header = [0u8; 512];
    reader
        .read_exact(&mut header)
        .context("Codex archive is truncated")?;
    let name = &header[..100];
    let computed: u64 = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            }
        })
        .sum();
    let valid = name.starts_with(member.as_bytes())
        && name[member.len()..].iter().all(|byte| *byte == 0)
        && matches!(header[156], b'0' | 0)
        && header[157..257].iter().all(|byte| *byte == 0)
        && header[257..262] == *b"ustar"
        && header[345..].iter().all(|byte| *byte == 0)
        && octal(&header[148..156]) == Some(computed);
    if !valid {
        bail!("Codex archive must contain only the regular file {member}");
    }
    let size = octal(&header[124..136])
        .filter(|size| (1..=BINARY_LIMIT).contains(size))
        .context("Codex archive member size is malformed or exceeds the runtime bound")?;
    let (digest, length) = copy_hashed(&mut (&mut reader).take(size), output)?;
    if length != size {
        bail!("Codex archive is truncated");
    }
    let mut rest = Vec::new();
    reader.take(TRAILER_LIMIT + 1).read_to_end(&mut rest)?;
    let padding = (512 - size % 512) % 512;
    let length = rest.len() as u64;
    if length > TRAILER_LIMIT || length < padding + 1024 || rest.iter().any(|byte| *byte != 0) {
        bail!("Codex archive has content beyond its single member");
    }
    Ok((digest, size))
}

fn octal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field)
        .ok()?
        .trim_matches(|ch| ch == '\0' || ch == ' ');
    if text.is_empty() || !text.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
        return None;
    }
    u64::from_str_radix(text, 8).ok()
}

/// The cosign executable, checked like every fixed host tool. Production
/// resolves `/usr/bin/cosign` from `/` as root; tests use a fixture tree.
fn cosign(source: &Source, layout: &Layout) -> Result<PathBuf> {
    let root = Path::new(layout.trusted_root);
    let path = root.join(source.cosign.trim_start_matches('/'));
    if fs::symlink_metadata(&path).is_err() {
        bail!("cosign is required to verify Codex releases: sudo apt install cosign");
    }
    resolve_trusted_executable(
        root,
        Path::new(source.cosign),
        layout.owner.0,
        SYSTEM_LINK_HOPS,
    )
    .context("cosign is not a root-controlled executable")?;
    Ok(path)
}

/// `--offline` only forbids falling back to an online Rekor search: the
/// bundle's signed entry timestamp is still verified against the Rekor key
/// from Sigstore's TUF trusted root, which cosign refreshes from its embedded
/// root into this private, throwaway `HOME`. A bundle without that proof fails.
fn verify_blob(
    cosign: &Path,
    work: &Path,
    bundle: &Path,
    binary: &Path,
    version: &str,
) -> Result<()> {
    let home = work.join("cosign-home");
    DirBuilder::new().mode(0o700).create(&home)?;
    let mut child = Command::new(cosign)
        .args(["verify-blob", "--offline=true", "--bundle"])
        .arg(bundle)
        .arg("--certificate-identity")
        .arg(format!("{IDENTITY_PREFIX}{version}"))
        .args(["--certificate-oidc-issuer", ISSUER])
        .arg(binary)
        .env_clear()
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start cosign")?;
    let deadline = Instant::now() + COSIGN_TIMEOUT;
    let verified = loop {
        if let Some(status) = child.try_wait()? {
            break status.success();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        thread::sleep(Duration::from_millis(100));
    };
    if !verified {
        bail!("cosign did not verify the Codex {version} release signature");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashMap,
        io::{BufRead, BufReader},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Mutex},
    };

    const TARGET: &str = "x86_64-unknown-linux-musl";
    const MEMBER: &str = "codex-x86_64-unknown-linux-musl";

    enum Response {
        Body(Vec<u8>),
        Redirect(String),
    }

    /// Minimal HTTP/1.1 server so curl's real redirect handling is exercised.
    fn serve(routes: Arc<Mutex<HashMap<String, Response>>>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split(' ').nth(1).unwrap_or_default().to_owned();
                while reader.read_line(&mut line).unwrap() > 0 && !line.ends_with("\r\n\r\n") {}
                let routes = routes.lock().unwrap();
                let (head, body): (String, &[u8]) = match routes.get(&path) {
                    Some(Response::Body(body)) => ("200 OK".to_owned(), body),
                    Some(Response::Redirect(to)) => (format!("302 Found\r\nLocation: {to}"), b""),
                    None => ("404 Not Found".to_owned(), b""),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
        });
        port
    }

    fn sha256(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn bundle(binary_digest: &str) -> Vec<u8> {
        let entry = serde_json::json!({
            "apiVersion": "0.0.1",
            "kind": "hashedrekord",
            "spec": { "data": { "hash": { "algorithm": "sha256", "value": binary_digest } } }
        });
        serde_json::json!({
            "base64Signature": "c2ln",
            "cert": "Y2VydA==",
            "rekorBundle": {
                "SignedEntryTimestamp": "c2V0",
                "Payload": { "body": BASE64.encode(entry.to_string()), "integratedTime": 1, "logIndex": 1, "logID": "00" }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Rootless release tree, served over local HTTP, with a fake cosign that
    /// records its arguments and exits with the code in `cosign-exit`.
    struct Fixture {
        root: tempfile::TempDir,
        port: u16,
        routes: Arc<Mutex<HashMap<String, Response>>>,
        /// Base URL, target and rollback copy.
        paths: [String; 3],
        redirects: &'static [&'static str],
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
            for directory in ["bin", "lib", "tools", "work"] {
                fs::create_dir(root.path().join(directory)).unwrap();
                fs::set_permissions(
                    root.path().join(directory),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
            let at = |name: &str| root.path().join(name).to_str().unwrap().to_owned();
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nread code < '{}'\nexit \"$code\"\n",
                at("cosign-args"),
                at("cosign-exit")
            );
            fs::write(root.path().join("tools/cosign"), script).unwrap();
            fs::set_permissions(
                root.path().join("tools/cosign"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            fs::write(root.path().join("cosign-exit"), "0\n").unwrap();
            let routes = Arc::new(Mutex::new(HashMap::new()));
            let port = serve(Arc::clone(&routes));
            let base = format!("http://127.0.0.1:{port}");
            let prefix: &'static str = format!("{base}/assets/").leak();
            let redirects: &'static [&'static str] = Box::leak(Box::new([prefix]));
            let paths = [base, at("bin/codex"), at("lib/jarvis/codex.previous")];
            Self {
                root,
                port,
                routes,
                paths,
                redirects,
            }
        }

        fn layout(&self) -> Layout<'_> {
            Layout {
                target: &self.paths[1],
                previous: &self.paths[2],
                trusted_root: self.root.path().to_str().unwrap(),
                owner: unsafe { (libc::geteuid(), libc::getegid()) },
            }
        }

        fn source(&self) -> Source<'_> {
            Source {
                releases: "http://127.0.0.1:0/unused",
                downloads: "http://127.0.0.1:0/unused",
                protocol: "=http",
                redirects: self.redirects,
                cosign: "/tools/cosign",
            }
        }

        fn install(&self, request: &VersionRequest) -> Result<String> {
            let base = &self.paths[0];
            let source = Source {
                releases: &format!("{base}/api"),
                downloads: &format!("{base}/download"),
                ..self.source()
            };
            install(&self.layout(), &source, request, TARGET)
        }

        fn route(&self, path: &str, response: Response) {
            self.routes
                .lock()
                .unwrap()
                .insert(path.to_owned(), response);
        }

        fn archive(&self, members: &[(&str, &[u8])]) -> Vec<u8> {
            let dir = tempfile::tempdir().unwrap();
            for (name, bytes) in members {
                fs::write(dir.path().join(name), bytes).unwrap();
            }
            let output = dir.path().join("out.tar.gz");
            let status = Command::new("/usr/bin/tar")
                .arg("-C")
                .arg(dir.path())
                .arg("-czf")
                .arg(&output)
                .args(members.iter().map(|(name, _)| *name))
                .status()
                .unwrap();
            assert!(status.success());
            fs::read(output).unwrap()
        }

        /// Publishes `archive` and `bundle` as release `version`, with API
        /// metadata for `tag` and the given archive digest.
        fn publish(&self, version: &str, tag: &str, archive: &[u8], digest: &str, bundle: &[u8]) {
            let asset = |name: &str, bytes: &[u8], digest: &str| serde_json::json!({ "name": name, "size": bytes.len(), "digest": format!("sha256:{digest}") });
            let release = serde_json::json!({
                "tag_name": tag,
                "assets": [
                    asset(&format!("{MEMBER}.tar.gz"), archive, digest),
                    asset(&format!("{MEMBER}.sigstore"), bundle, &sha256(bundle)),
                    asset(&format!("{MEMBER}.zst"), b"other", &sha256(b"other")),
                ]
            });
            let body = release.to_string().into_bytes();
            self.route(&format!("/api/tags/rust-v{version}"), Response::Body(body));
            for (name, bytes) in [
                (format!("{MEMBER}.tar.gz"), archive),
                (format!("{MEMBER}.sigstore"), bundle),
            ] {
                let asset = format!("/assets/rust-v{version}/{name}");
                self.route(
                    &format!("/download/rust-v{version}/{name}"),
                    Response::Redirect(format!("http://127.0.0.1:{}{asset}", self.port)),
                );
                self.route(&asset, Response::Body(bytes.to_vec()));
            }
        }

        fn release(&self, version: &str, binary: &[u8]) {
            let archive = self.archive(&[(MEMBER, binary)]);
            let digest = sha256(&archive);
            self.publish(
                version,
                &format!("rust-v{version}"),
                &archive,
                &digest,
                &bundle(&sha256(binary)),
            );
        }

        fn latest(&self, version: &str) {
            let mut routes = self.routes.lock().unwrap();
            let Some(Response::Body(body)) = routes.get(&format!("/api/tags/rust-v{version}"))
            else {
                panic!("unpublished release");
            };
            let body = body.clone();
            routes.insert("/api/latest".to_owned(), Response::Body(body));
        }

        fn read(&self, index: usize) -> Option<Vec<u8>> {
            fs::read(&self.paths[index]).ok()
        }

        fn cosign_args(&self) -> Vec<String> {
            fs::read_to_string(self.root.path().join("cosign-args"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn assert_refused(&self, request: VersionRequest, expected: &str) {
            let before = (self.read(1), self.read(2));
            let error = self.install(&request).unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            assert_eq!((self.read(1), self.read(2)), before);
        }
    }

    fn exact(version: &str) -> VersionRequest {
        VersionRequest::Exact(version.to_owned())
    }

    #[test]
    fn production_source_and_destination_are_fixed() {
        assert_eq!(
            SOURCE.releases,
            "https://api.github.com/repos/openai/codex/releases"
        );
        assert_eq!(
            SOURCE.downloads,
            "https://github.com/openai/codex/releases/download"
        );
        assert_eq!(SOURCE.protocol, "=https");
        assert_eq!(SOURCE.cosign, "/usr/bin/cosign");
        assert!(SOURCE
            .redirects
            .iter()
            .all(|prefix| prefix.starts_with("https://")
                && prefix.ends_with(".githubusercontent.com/")));
        assert_eq!(LAYOUT.target, AccountProvider::Codex.binary());
        assert_eq!(LAYOUT.owner, (0, 0));
        assert_eq!(LAYOUT.trusted_root, "/");
    }

    #[test]
    fn real_release_bundle_names_the_binary_digest() {
        let real = include_bytes!(
            "../../../tests/fixtures/codex-0.160.0-x86_64-unknown-linux-musl.sigstore"
        );
        assert_eq!(
            rekor_digest(real).unwrap(),
            "12eb3e81114588aca3b7998f4f19e8997b056aca08e57a7ca7c8a3ec8c652aad"
        );
        assert!(rekor_digest(b"{}").is_err());
        assert!(rekor_digest(&bundle(&"A".repeat(64))).is_err());
    }

    #[test]
    fn redirects_only_reach_allowed_prefixes() {
        let source = SOURCE;
        let ok = "https://release-assets.githubusercontent.com/github-production-release-asset/1/x?sp=r&sig=a%3D";
        assert_eq!(allowed_redirect(&source, ok).unwrap(), ok);
        for bad in [
            "",
            "http://objects.githubusercontent.com/x",
            "https://objects.githubusercontent.com.evil.example/x",
            "https://objects.githubusercontent.com@evil.example/x",
            "https://objects.githubusercontent.com:8443/x",
            "https://evil.example/https://objects.githubusercontent.com/x",
            "https://objects.githubusercontent.com/x y",
            "https://objects.githubusercontent.com/x\n",
        ] {
            assert!(allowed_redirect(&source, bad).is_err(), "{bad}");
        }
    }

    fn header(name: &str, size: u64, kind: u8) -> [u8; 512] {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000755\0");
        header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[156] = kind;
        header[257..265].copy_from_slice(b"ustar  \0");
        header[148..156].fill(b' ');
        let sum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        header
    }

    fn tar(entries: &[([u8; 512], &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (header, data) in entries {
            bytes.extend_from_slice(header);
            bytes.extend_from_slice(data);
            bytes.resize(bytes.len().div_ceil(512) * 512, 0);
        }
        bytes.resize(bytes.len() + 1024, 0);
        bytes
    }

    #[test]
    fn archive_must_hold_only_the_expected_regular_file() {
        let body = b"codex-binary";
        let good = tar(&[(header(MEMBER, 12, b'0'), body)]);
        let mut out = Vec::new();
        assert_eq!(
            untar(&good[..], MEMBER, &mut out).unwrap(),
            (sha256(body), 12)
        );
        assert_eq!(out, body);
        let refused = |bytes: Vec<u8>| untar(&bytes[..], MEMBER, &mut Vec::new()).is_err();
        for (name, kind) in [
            ("/codex-x86_64-unknown-linux-musl", b'0'),
            ("../codex-x86_64-unknown-linux-musl", b'0'),
            ("./codex-x86_64-unknown-linux-musl", b'0'),
            ("codex-x86_64-unknown-linux-musl/x", b'0'),
            ("codex", b'0'),
            (MEMBER, b'1'),
            (MEMBER, b'2'),
            (MEMBER, b'5'),
            (MEMBER, b'x'),
            (MEMBER, b'L'),
        ] {
            assert!(
                refused(tar(&[(header(name, 12, kind), body)])),
                "{name} {kind}"
            );
        }
        // Symlink with a link target, and an extension header before the member.
        let mut link = header(MEMBER, 0, b'2');
        link[157..161].copy_from_slice(b"/etc");
        assert!(refused(tar(&[(link, b"")])));
        assert!(refused(tar(&[
            (header("././@PaxHeader", 12, b'x'), b"12 path=x\n\0\0"),
            (header(MEMBER, 12, b'0'), body)
        ])));
        // A second member, trailing junk, truncation and a bad checksum.
        assert!(refused(tar(&[
            (header(MEMBER, 12, b'0'), body),
            (header("evil", 4, b'0'), b"evil")
        ])));
        let mut junk = good.clone();
        junk.push(1);
        assert!(refused(junk));
        assert!(refused(good[..good.len() - 1024].to_vec()));
        assert!(refused(good[..300].to_vec()));
        let mut checksum = good.clone();
        checksum[0] = b'C';
        assert!(refused(checksum));
        // Size beyond the runtime bound is refused before reading the data.
        assert!(refused(tar(&[(
            header(MEMBER, BINARY_LIMIT + 1, b'0'),
            b""
        )])));
    }

    #[test]
    fn verified_release_installs_keeps_previous_and_rolls_back() {
        let fixture = Fixture::new();
        fixture.release("0.160.0", b"codex-160");
        fixture.latest("0.160.0");
        let layout = fixture.layout();
        assert_eq!(
            fixture
                .install(&VersionRequest::Channel(Channel::Latest))
                .unwrap(),
            "0.160.0"
        );
        assert_eq!(fixture.read(1).as_deref(), Some(&b"codex-160"[..]));
        let mode = fs::metadata(layout.target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        assert_eq!(fixture.read(2), None);
        let args = fixture.cosign_args();
        let (bundle, binary) = (&args[3], &args[8]);
        assert_eq!(
            args,
            [
                "verify-blob",
                "--offline=true",
                "--bundle",
                bundle,
                "--certificate-identity",
                "https://github.com/openai/codex/.github/workflows/rust-release.yml@refs/tags/rust-v0.160.0",
                "--certificate-oidc-issuer",
                "https://token.actions.githubusercontent.com",
                binary,
            ]
        );
        assert!(bundle.ends_with("/codex.sigstore") && binary.ends_with("/codex"));

        fixture.release("0.161.0", b"codex-161");
        assert_eq!(fixture.install(&exact("0.161.0")).unwrap(), "0.161.0");
        assert_eq!(fixture.read(1).as_deref(), Some(&b"codex-161"[..]));
        assert_eq!(fixture.read(2).as_deref(), Some(&b"codex-160"[..]));
        // Reinstalling the active version keeps the real rollback copy.
        assert_eq!(fixture.install(&exact("0.161.0")).unwrap(), "0.161.0");
        assert_eq!(fixture.read(2).as_deref(), Some(&b"codex-160"[..]));
        // The work directory lived in the rollback directory and is gone.
        let lib = Path::new(layout.previous).parent().unwrap();
        assert_eq!(fs::read_dir(lib).unwrap().count(), 1);

        super::super::rollback(&layout).unwrap();
        assert_eq!(fixture.read(1).as_deref(), Some(&b"codex-160"[..]));
        assert_eq!(fixture.read(2), None);
        assert!(super::super::rollback(&layout).is_err());
    }

    #[test]
    fn unverifiable_releases_are_refused_without_changes() {
        let fixture = Fixture::new();
        fixture.release("0.160.0", b"codex-160");
        fixture.install(&exact("0.160.0")).unwrap();

        // cosign rejects the signature.
        fixture.release("0.161.0", b"codex-161");
        fs::write(fixture.root.path().join("cosign-exit"), "1\n").unwrap();
        fixture.assert_refused(exact("0.161.0"), "cosign did not verify");
        fs::write(fixture.root.path().join("cosign-exit"), "0\n").unwrap();

        // Archive does not match GitHub's digest.
        let archive = fixture.archive(&[(MEMBER, b"codex-162")]);
        let signed = bundle(&sha256(b"codex-162"));
        fixture.publish(
            "0.162.0",
            "rust-v0.162.0",
            &archive,
            &"0".repeat(64),
            &signed,
        );
        fixture.assert_refused(exact("0.162.0"), "digest");

        // Binary does not match the signed Rekor entry.
        let digest = sha256(&archive);
        fixture.publish(
            "0.163.0",
            "rust-v0.163.0",
            &archive,
            &digest,
            &bundle(&sha256(b"x")),
        );
        fixture.assert_refused(exact("0.163.0"), "Rekor");

        // A second member in the archive.
        let extra = fixture.archive(&[(MEMBER, b"codex-164"), ("evil", b"evil")]);
        fixture.publish("0.164.0", "rust-v0.164.0", &extra, &sha256(&extra), &signed);
        fixture.assert_refused(exact("0.164.0"), "content beyond");

        // Metadata for another release served under the requested tag.
        fixture.publish("0.165.0", "rust-v0.160.0", &archive, &digest, &signed);
        fixture.assert_refused(exact("0.165.0"), "different Codex release");

        // Redirect to a host outside the allowlist.
        fixture.release("0.166.0", b"codex-166");
        fixture.route(
            &format!("/download/rust-v0.166.0/{MEMBER}.sigstore"),
            Response::Redirect("http://127.0.0.2:1/x".to_owned()),
        );
        fixture.assert_refused(exact("0.166.0"), "redirected outside");

        // Served archive larger than its published size.
        fixture.release("0.167.0", b"codex-167");
        let path = format!("/assets/rust-v0.167.0/{MEMBER}.tar.gz");
        fixture.route(&path, Response::Body(vec![0; 64 * 1024]));
        fixture.assert_refused(exact("0.167.0"), "download failed");

        // Published size above the runtime bound, refused before download.
        let release = serde_json::json!({ "tag_name": "rust-v0.168.0", "assets": [
            { "name": format!("{MEMBER}.sigstore"), "size": 10, "digest": format!("sha256:{digest}") },
            { "name": format!("{MEMBER}.tar.gz"), "size": BINARY_LIMIT + 1, "digest": format!("sha256:{digest}") }
        ]});
        fixture.route(
            "/api/tags/rust-v0.168.0",
            Response::Body(release.to_string().into_bytes()),
        );
        fixture.assert_refused(exact("0.168.0"), "exceeds its bound");

        // Version requests that never reach the network.
        fixture.assert_refused(exact("0.169.0-alpha.1"), "strict");
        fixture.assert_refused(exact("v0.169.0"), "strict");
        fixture.assert_refused(
            VersionRequest::Channel(Channel::Stable),
            "no stable channel",
        );
        fixture.route(
            "/api/latest",
            Response::Body(br#"{"tag_name":"rust-v0.170.0-alpha.1","assets":[]}"#.to_vec()),
        );
        fixture.assert_refused(VersionRequest::Channel(Channel::Latest), "rust-vMAJOR");

        // Missing cosign is refused before any download.
        fs::remove_file(fixture.root.path().join("tools/cosign")).unwrap();
        fixture.assert_refused(exact("0.161.0"), "sudo apt install cosign");
        assert_eq!(fixture.read(1).as_deref(), Some(&b"codex-160"[..]));
    }
}
