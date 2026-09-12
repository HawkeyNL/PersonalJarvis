use super::*;
use crate::mirror::release::tests::Fixture;
use std::io::Write;

fn dirs() -> (TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("protected");
    let public = temp.path().join("public");
    for path in [&root, &public, &public.join("ios")] {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (temp, root, public)
}
fn prepare(store: &ReleaseStore, f: &Fixture, public: &Path) -> Result<PathBuf> {
    let stage = store.stage()?;
    for (name, bytes) in &f.files {
        store.stage_file(&stage, name)?.write_all(bytes).unwrap();
    }
    // Test-only APK bytes are not installable. Production prepare() always runs
    // the fixed apksigner; this private helper tests crypto/storage in isolation.
    store.prepare_verified(stage, &f.config, &f.oci, public)
}

#[test]
fn complete_signed_release_stages_and_activates_exact_api_layout() {
    let (_temp, root, public) = dirs();
    let owner = unsafe { libc::geteuid() };
    let store = ReleaseStore::open(&root, owner).unwrap();
    let f = Fixture::new("0.1.0", 1);
    let target = prepare(&store, &f, &public).unwrap();
    assert!(!root.join("current").exists());
    store.activate(&target).unwrap();
    assert_eq!(
        fs::read_link(root.join("current")).unwrap(),
        Path::new("releases/v0.1.0")
    );
    for (platform, suffix) in [
        ("linux-x86_64", ".AppImage"),
        ("windows-x86_64", ".exe"),
        ("macos-arm64", ".app.tar.gz"),
        ("android-universal", ".apk"),
    ] {
        let name = format!("Jarvis_0.1.0_{}{suffix}", platform.replace('-', "_"));
        assert_eq!(
            fs::read(root.join("current").join(platform).join(&name)).unwrap(),
            f.files[&name]
        );
        assert_eq!(
            fs::read(public.join("releases/v0.1.0").join(platform).join(&name)).unwrap(),
            f.files[&name]
        );
    }
    assert_eq!(
        fs::read(root.join("current/manifest.json")).unwrap(),
        f.files["latest.json"]
    );
    for path in [
        root.join("current/manifest.json"),
        public.join("releases/v0.1.0/android-universal/Jarvis_0.1.0_android_universal.apk"),
    ] {
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o644);
    }
    let index = super::super::Store::open(&public, owner).unwrap();
    index.render_index().unwrap();
    let html = fs::read_to_string(public.join("index.html")).unwrap();
    for label in [
        "Linux",
        "Windows",
        "macOS",
        "Android",
        "iOS — zelf ondertekenen",
    ] {
        assert!(html.contains(label));
    }
    assert!(!html.contains("fixture-private-token"));
    assert!(prepare(&store, &f, &public).is_ok());
}

#[test]
fn failure_downgrade_and_changed_version_preserve_active_generation() {
    let (_temp, root, public) = dirs();
    let store = ReleaseStore::open(&root, unsafe { libc::geteuid() }).unwrap();
    let f = Fixture::new("0.2.0", 2);
    store
        .activate(&prepare(&store, &f, &public).unwrap())
        .unwrap();
    for next in [
        Fixture::new("0.1.0", 1),
        Fixture::new("0.3.0", 2),
        Fixture::new("0.2.0", 3),
    ] {
        assert!(prepare(&store, &next, &public).is_err());
        assert_eq!(
            fs::read_link(root.join("current")).unwrap(),
            Path::new("releases/v0.2.0")
        );
    }
    let mut tampered = Fixture::new("0.3.0", 3);
    tampered
        .files
        .get_mut("Jarvis_0.3.0_linux_x86_64.AppImage")
        .unwrap()
        .push(1);
    assert!(prepare(&store, &tampered, &public).is_err());
    assert!(!root.join("releases/v0.3.0").exists());
    assert!(fs::read_dir(&root).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_str()
        .unwrap()
        .starts_with(".release-staging-")));
}

#[test]
fn symlinks_escaping_current_and_concurrent_writers_rejected() {
    let (temp, root, public) = dirs();
    let owner = unsafe { libc::geteuid() };
    let store = ReleaseStore::open(&root, owner).unwrap();
    assert!(ReleaseStore::open(&root, owner).is_err());
    symlink(temp.path(), root.join("current")).unwrap();
    assert!(prepare(&store, &Fixture::new("0.1.0", 1), &public).is_err());
    fs::remove_file(root.join("current")).unwrap();
    symlink(temp.path(), root.join("releases/v0.1.0")).unwrap();
    assert!(prepare(&store, &Fixture::new("0.1.0", 1), &public).is_err());
}
