//! Release-only verifier for the exact updater artifacts staged by CI.

use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let artifact = PathBuf::from(arguments.next().ok_or("artifact path is required")?);
    let signature = PathBuf::from(arguments.next().ok_or("signature path is required")?);
    if arguments.next().is_some() {
        return Err("unexpected verifier argument".into());
    }
    let public_key = std::env::var("JARVIS_TAURI_UPDATER_PUBKEY")
        .map_err(|_| "JARVIS_TAURI_UPDATER_PUBKEY is required")?;
    // Tauri stores both the complete minisign public-key document and the
    // complete signature document as base64 strings. Decode exactly like the
    // updater plugin does before verifying the staged bytes.
    let public_key = base64::engine::general_purpose::STANDARD.decode(public_key.trim())?;
    let public_key = PublicKey::decode(std::str::from_utf8(&public_key)?)?;
    let signature = std::fs::read_to_string(signature)?;
    let signature = base64::engine::general_purpose::STANDARD.decode(signature.trim())?;
    let signature = Signature::decode(std::str::from_utf8(&signature)?)?;
    let mut verifier = public_key.verify_stream(&signature)?;
    let mut source = File::open(artifact)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier.finalize()?;
    Ok(())
}
