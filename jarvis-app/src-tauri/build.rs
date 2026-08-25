fn main() {
    // On macOS, `tauri dev` runs an *unbundled* binary — no .app, no Info.plist —
    // so WKWebView refuses to expose `navigator.mediaDevices` and the mic is
    // "unavailable" (voice enroll/verify + "Hey Jarvis"). Embed the Info.plist
    // into the dev binary's Mach-O `__info_plist` section so the process carries
    // the NSMicrophoneUsageDescription even when run without a bundle.
    // Release `.app` bundles read Contents/Info.plist instead, so scope this to
    // macOS debug builds only.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();
    if target_os == "macos" && profile == "debug" {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        println!("cargo:rerun-if-changed=Info.plist");
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{manifest}/Info.plist"
        );
    }
    tauri_build::build()
}
