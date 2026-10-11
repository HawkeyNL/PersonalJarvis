# Home Node disk housekeeping

`jarvis-housekeeping` is a release-shipped, root-only helper that frees disk
space taken by old Jarvis release generations. Every decision is a fixed rule
over strict directory names. No agent, model or caller input chooses what is
deleted, and the helper accepts no path or pattern.

```bash
sudo jarvis housekeeping status          # read-only plan, sizes, disk free, last run
sudo jarvis housekeeping status --json   # the same as stable JSON
sudo jarvis housekeeping run             # dry run: identical to status
sudo jarvis housekeeping run --apply     # delete what status lists
```

## What is deleted

Only two kinds of directory, and only when every check passes:

| Location | Name | Kept |
| --- | --- | --- |
| `/opt/jarvis/releases/` | `vX.Y.Z` | the active release (`/opt/jarvis/current`); every newer release (staged for a migration); the updater's own rollback target; the newest 3; any release named by a symlink below `/opt/jarvis`; any release installed or staged in the last 7 days; any directory not owned by root |
| `/var/lib/jarvis-app-updates/releases/` | `vX.Y.Z` | the active generation (`current`); every newer generation; the newest 3; any generation named by a symlink in the mirror; any installed in the last 7 days; any directory not owned by root |

The rollback target is not guessed: the helper asks the active release's
updater (`update-core-release --rollback-candidates`) and keeps the first
rollback-capable entry, the release `jarvis update --rollback` would use. It
also keeps any newer legacy release that only lacks its verification marker,
because the updater may migrate and use one of those first. "Installed in the
last 7 days" uses the directory's inode change time: extraction restores the
archive's old modification time, but a fresh install or staging cannot look
old.

Names outside the strict `vMAJOR.MINOR.PATCH` form (`.staging.*`,
`.release-staging-*`, `v1.2`, `v0.0.3.old`, …) are never touched. Symlinks are
never followed or removed: a symlink named like a release is ignored, and a
symlink inside a deleted release is removed as a link, not followed.

## What is never deleted

These are reported by `status` only:

- **Public downloads** (`/var/lib/jarvis-public-downloads`). `jarvis-app-downloads
  sync-release` retires old releases itself under the owner retention policy
  (see `crates/app-downloads/README.md`); housekeeping never deletes there.
- **The legacy app mirror** (`/var/lib/jarvis/app-updates`). The owner decides
  whether to remove it.
- **Docker.** Dangling images are counted. They cannot be attributed to Jarvis
  on a daemon shared with other services, so nothing is pruned: no
  `docker system prune`, no images, containers or volumes.
- **Logs.** Jarvis services log to the systemd journal, which other services
  share; it is never vacuumed. Files matching `/var/log/jarvis*` are counted.
- Anything outside the two directories above, including all non-Jarvis
  services, containers, images, volumes, files and logs on the machine.

## Safety

- **Locks.** `apply` takes the housekeeping lock, the Core updater lock (which
  also excludes a running backup) and the app-mirror sync lock, all
  non-blocking. If any is held it exits with code **75** before deleting
  anything; the timer retries after 10 minutes (at most 3 starts in 2 hours).
  A rule error (exit 1) is not retried.
  `status` changes nothing and takes no lock of its own. It does run the
  updater's read-only `--rollback-candidates`, which holds the updater lock
  while it verifies every installed release; a `jarvis-updater.service` run
  that starts in that window exits 75 and simply retries five minutes later.
- **Fail closed per rule.** An unexpected layout (a missing or non-canonical
  `current` link, unavailable or inconsistent updater candidates, an unsafe
  lock file) skips that rule, is reported as `error` and makes the run exit 1.
- **Audit.** Each deletion records fixed fields through syslog
  `authpriv.notice` with tag `jarvis-housekeeping`, before and after:
  `rule=… action=delete name=vX.Y.Z bytes=N outcome=started|removed|failed`.
  If the first record cannot be written, nothing is deleted.
  Read them with `sudo journalctl -t jarvis-housekeeping`.
- **Last run.** `apply` writes `/var/lib/jarvis-housekeeping/last-run.json`
  (root, mode 0644): time, outcome, number removed, bytes reclaimed and disk
  free. Core Admin shows it on the System page; `verify-home-node` checks its
  permissions.

## Daily schedule

Core releases ship `jarvis-housekeeping.timer`, **disabled**. Installers and
updates never enable it. Review one plan first, then enable it:

```bash
sudo jarvis housekeeping status
sudo systemctl enable --now jarvis-housekeeping.timer
systemctl list-timers jarvis-housekeeping.timer
```

The timer runs daily at 04:30 local time plus a random delay of up to one
hour, at idle CPU and lowest best-effort I/O priority, before the 06:00
backup window. Reports run before any lock is taken, so the updater lock is
held only while planning and deleting. A run
missed while the machine was off runs at the next boot. The schedule is
fixed: a drop-in that changes it blocks Core updates until it is removed.

The service is sandboxed: no network, only `CAP_DAC_READ_SEARCH`, and only
the updater and housekeeping lock files below `/run`, `/opt/jarvis/releases`,
`/var/lib/jarvis-app-updates` and its own state directory are writable.

## Disable

```bash
sudo systemctl disable --now jarvis-housekeeping.timer
```

Nothing else needs undoing: the helper keeps no state besides the last-run
record. After a rollback to a Core release without the helper, an enabled
timer stays installed but its service is skipped (`ConditionPathExists`), so
it neither runs nor fails until a newer release returns.
