use anyhow::{bail, Context, Result};
use clap::Subcommand;
use jarvis_privileged::local_devices::{DeviceRequest, RESPONSE_LIMIT, SOCKET_PATH};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    List,
    Pending,
    Approve {
        request_id: Uuid,
        #[arg(long)]
        fingerprint: String,
    },
    Deny {
        request_id: Uuid,
    },
    Revoke {
        device_id: Uuid,
    },
}

pub fn run(command: DeviceCommand, json: bool) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("local device administration requires the host owner root path");
    }
    let request = match command {
        DeviceCommand::List => DeviceRequest::List {},
        DeviceCommand::Pending => DeviceRequest::Pending {},
        DeviceCommand::Approve {
            request_id,
            fingerprint,
        } => DeviceRequest::Approve {
            request_id,
            fingerprint,
        },
        DeviceCommand::Deny { request_id } => DeviceRequest::Deny { request_id },
        DeviceCommand::Revoke { device_id } => DeviceRequest::Revoke { device_id },
    };
    if json && !matches!(request, DeviceRequest::List {} | DeviceRequest::Pending {}) {
        bail!("JSON mode is read-only");
    }
    let mut bytes = serde_json::to_vec(&request)?;
    DeviceRequest::parse(&bytes).map_err(anyhow::Error::msg)?;
    let account = unsafe { libc::getpwnam(c"jarvis".as_ptr()) };
    if account.is_null() {
        bail!("Jarvis service account unavailable");
    }
    let uid = unsafe { (*account).pw_uid };
    let parent = fs::symlink_metadata("/run/jarvis-core-admin")
        .context("local device administration unavailable; check Core and release-matched units")?;
    let socket = fs::symlink_metadata(SOCKET_PATH)?;
    if !parent.is_dir()
        || parent.uid() != uid
        || parent.mode() & 0o077 != 0
        || !socket.file_type().is_socket()
        || socket.uid() != uid
        || socket.mode() & 0o177 != 0
    {
        bail!("unsafe local device administration socket");
    }
    let mut stream = UnixStream::connect(SOCKET_PATH)?;
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut size,
        )
    } != 0
        || size as usize != std::mem::size_of::<libc::ucred>()
        || peer.uid != uid
    {
        bail!("unexpected local device administration peer");
    }
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    let mut response = Vec::new();
    BufReader::new(stream.take((RESPONSE_LIMIT + 1) as u64)).read_until(b'\n', &mut response)?;
    if response.len() > RESPONSE_LIMIT {
        bail!("local device response too large");
    }
    let response: serde_json::Value = serde_json::from_slice(&response)?;
    if response.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        bail!(
            "device operation refused; refresh pending requests/devices and verify your selection"
        );
    }
    let data = response
        .get("data")
        .context("invalid local device response")?;
    // JSON escaping also makes control characters in remote device names safe
    // in ordinary terminal output. Never initialize a transient TUI here.
    println!("{}", serde_json::to_string_pretty(data)?);
    Ok(())
}
