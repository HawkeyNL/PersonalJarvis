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
fn download_version_picker_defaults_to_semver_latest_without_scripts() {
    let (_temp, root, public) = dirs();
    let owner = unsafe { libc::geteuid() };
    let store = ReleaseStore::open(&root, owner).unwrap();
    for (version, build) in [("0.1.9", 1), ("0.1.10", 2)] {
        prepare(&store, &Fixture::new(version, build), &public).unwrap();
    }
    super::super::Store::open(&public, owner)
        .unwrap()
        .render_index()
        .unwrap();
    let html = fs::read_to_string(public.join("index.html")).unwrap();
    assert!(html.contains("<option value=\"0.1.10\" selected>v0.1.10 — Latest</option>"));
    assert!(html.contains("<option value=\"0.1.9\">v0.1.9</option>"));
    assert_eq!(html.matches(" selected>").count(), 1);
    assert_eq!(html.matches("class=\"latest-badge\"").count(), 1);
    for version in ["0.1.9", "0.1.10"] {
        assert!(html.contains(&format!("option[value=\"{version}\"]:checked) .client-release:not([data-version=\"{version}\"])")));
        assert!(html.contains(&format!(
            "class=\"client-release\" data-version=\"{version}\""
        )));
        assert!(html.contains(&format!("/downloads/releases/v{version}/ios-arm64/")));
    }
    assert!(html.contains("for=\"release-version\""));
    assert!(!html.contains("<script"));
    assert!(!html.contains("onchange="));
}

#[test]
fn retirement_removes_old_public_releases_after_the_index_drops_them() {
    let (_temp, root, public) = dirs();
    let owner = unsafe { libc::geteuid() };
    let store = ReleaseStore::open(&root, owner).unwrap();
    for patch in 1..=5 {
        prepare(
            &store,
            &Fixture::new(&format!("0.1.{patch}"), patch),
            &public,
        )
        .unwrap();
    }
    let index = super::super::Store::open(&public, owner).unwrap();
    index.render_index().unwrap();
    assert_eq!(index.retire_releases().unwrap(), 2);
    let html = fs::read_to_string(public.join("index.html")).unwrap();
    for patch in 1..=5 {
        let version = format!("0.1.{patch}");
        let present = patch >= 3;
        assert_eq!(
            public.join(format!("releases/v{version}")).exists(),
            present
        );
        assert_eq!(
            html.contains(&format!("data-version=\"{version}\"")),
            present
        );
        // The authenticated update mirror is never pruned by public retention.
        assert!(root.join(format!("releases/v{version}")).exists());
    }
    assert!(fs::read_dir(&public).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".retired-")));
    assert_eq!(index.retire_releases().unwrap(), 0);
}

#[test]
fn failed_retirement_restores_releases_and_sweeps_crash_leftovers() {
    let (_temp, root, public) = dirs();
    let owner = unsafe { libc::geteuid() };
    let store = ReleaseStore::open(&root, owner).unwrap();
    for patch in 1..=4 {
        prepare(
            &store,
            &Fixture::new(&format!("0.1.{patch}"), patch),
            &public,
        )
        .unwrap();
    }
    let index = super::super::Store::open(&public, owner).unwrap();
    // An invalid iOS candidate makes the index rebuild fail after the moves.
    fs::create_dir(public.join("ios/junk")).unwrap();
    assert!(index.retire_releases().is_err());
    for patch in 1..=4 {
        assert!(public.join(format!("releases/v0.1.{patch}")).exists());
    }
    fs::remove_dir(public.join("ios/junk")).unwrap();
    fs::create_dir_all(public.join(".retired-crash/v0.0.1")).unwrap();
    fs::write(public.join(".retired-crash/v0.0.1/installer"), b"x").unwrap();
    assert_eq!(index.retire_releases().unwrap(), 1);
    assert!(!public.join(".retired-crash").exists());
    assert!(!public.join("releases/v0.1.1").exists());
    assert!(public.join("ios").exists());
}

#[test]
fn retirement_fails_closed_on_an_unexpected_public_entry() {
    let (_temp, root, public) = dirs();
    let owner = unsafe { libc::geteuid() };
    let store = ReleaseStore::open(&root, owner).unwrap();
    for patch in 1..=4 {
        prepare(
            &store,
            &Fixture::new(&format!("0.1.{patch}"), patch),
            &public,
        )
        .unwrap();
    }
    let index = super::super::Store::open(&public, owner).unwrap();
    for name in ["notes", "v0.1.9-rc.1"] {
        fs::create_dir(public.join("releases").join(name)).unwrap();
        assert!(index.retire_releases().is_err());
        assert!(public.join("releases/v0.1.1").exists());
        fs::remove_dir(public.join("releases").join(name)).unwrap();
    }
    symlink(&root, public.join("releases/v0.1.9")).unwrap();
    assert!(index.retire_releases().is_err());
    assert!(public.join("releases/v0.1.1").exists());
    assert!(root.join("releases/v0.1.1").exists());
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
    for label in ["Linux", "Windows", "macOS", "Android", "iOS — self-signing"] {
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
