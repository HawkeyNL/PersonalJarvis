use jarvis_app_downloads::{plan, Entry, PlanError, Target, MAX_CATALOG_ENTRIES};

fn entry(target: Target, version: impl Into<String>) -> Entry {
    Entry {
        target,
        version: version.into(),
    }
}

fn line(major: u64, minor: u64, through: u64) -> Vec<Entry> {
    (0..=through)
        .map(|patch| entry(Target::LinuxX86_64, format!("{major}.{minor}.{patch}")))
        .collect()
}

fn kept(entries: Vec<Entry>) -> Vec<String> {
    plan(entries)
        .unwrap()
        .remove(0)
        .keep
        .into_iter()
        .map(|v| v.version)
        .collect()
}

#[test]
fn owner_examples_follow_the_evolving_catalog() {
    let mut inventory = line(1, 0, 8);
    assert_eq!(
        kept(inventory.clone()),
        ["1.0.0", "1.0.6", "1.0.7", "1.0.8"]
    );
    inventory.extend(line(2, 0, 0));
    assert_eq!(
        kept(inventory.clone()),
        ["1.0.0", "1.0.7", "1.0.8", "2.0.0"]
    );
    inventory.extend((1..=5).map(|p| entry(Target::LinuxX86_64, format!("2.0.{p}"))));
    assert_eq!(
        kept(inventory.clone()),
        ["1.0.0", "2.0.0", "2.0.3", "2.0.4", "2.0.5"]
    );
    inventory.extend(line(2, 1, 5));
    assert_eq!(
        kept(inventory.clone()),
        ["1.0.0", "2.0.0", "2.0.5", "2.1.3", "2.1.4", "2.1.5"]
    );
    inventory.extend(line(3, 0, 5));
    inventory.extend(line(4, 0, 0));
    assert_eq!(
        kept(inventory),
        ["2.0.0", "3.0.0", "3.0.4", "3.0.5", "4.0.0"]
    );
}

#[test]
fn only_latest_three_minor_lines_in_current_major_survive() {
    let entries = (0..=4).flat_map(|minor| line(2, minor, 5)).collect();
    assert_eq!(
        kept(entries),
        ["2.0.0", "2.2.5", "2.3.5", "2.4.3", "2.4.4", "2.4.5"]
    );
}

#[test]
fn each_target_has_its_own_latest_release() {
    let mut entries = line(4, 0, 5);
    entries.extend(line(1, 0, 8).into_iter().map(|mut e| {
        e.target = Target::AndroidUniversal;
        e
    }));
    let result = plan(entries).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[1].keep.last().unwrap().version, "1.0.8");
    assert!(!result[1].keep.is_empty());
}

#[test]
fn numeric_order_not_lexical_and_no_invented_baseline() {
    let entries = ["2.0.9", "2.0.10", "2.0.11", "2.0.12"].map(|v| entry(Target::LinuxX86_64, v));
    assert_eq!(kept(entries.to_vec()), ["2.0.10", "2.0.11", "2.0.12"]);
    let reversed = entries.into_iter().rev().collect();
    assert_eq!(kept(reversed), ["2.0.10", "2.0.11", "2.0.12"]);
}

#[test]
fn invalid_catalog_never_produces_a_partial_removal_plan() {
    for version in [
        "v1.0.0",
        "app-v1.0.0",
        "1.00.0",
        "1.0.0-rc.1",
        "1.0.0+build",
        "../1.0.0",
        "1.0.0\n",
    ] {
        assert_eq!(
            plan([entry(Target::LinuxX86_64, version)]),
            Err(PlanError::InvalidVersion)
        );
    }
    let duplicate = entry(Target::LinuxX86_64, "1.0.0");
    assert_eq!(
        plan([duplicate.clone(), duplicate]),
        Err(PlanError::Duplicate)
    );
    let huge = (0..=MAX_CATALOG_ENTRIES).map(|p| entry(Target::LinuxX86_64, format!("1.0.{p}")));
    assert_eq!(plan(huge), Err(PlanError::TooManyEntries));
    assert_eq!(plan([]), Ok(vec![]));
}

#[test]
fn incremental_cleanup_matches_a_full_history_plan() {
    let mut retained = vec![];
    let mut full = vec![];
    for major in 1..=4 {
        for minor in 0..=4 {
            for patch in 0..=8 {
                let incoming = entry(Target::LinuxX86_64, format!("{major}.{minor}.{patch}"));
                full.push(incoming.clone());
                retained.push(incoming);
                let expected = kept(full.clone());
                let actual = kept(retained);
                assert_eq!(actual, expected);
                retained = actual
                    .into_iter()
                    .map(|v| entry(Target::LinuxX86_64, v))
                    .collect();
            }
        }
    }
}
