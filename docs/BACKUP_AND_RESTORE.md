# Home Node backup and restore

`jarvis-backup` writes one encrypted archive per run for disaster recovery.
The owner runs it as root, by hand or with the opt-in daily timer. Nothing is
uploaded. For a backup that stays on the Home Node, see
[Local-only backup](#local-only-backup).

> **Archives stay on the same disk as the live data.** Each run keeps the
> newest 7 archives and the newest archive of each of the last 4 ISO weeks
> (3 to 4 weeks of history). A disk failure or a compromised host still
> leaves you with nothing. Copy archives off the machine when you can (see
> [Copying off the machine](#copying-off-the-machine)).

## What is in an archive

`jarvis-backup-YYYY-MM-DD.tar` (mode 0600) and `jarvis-backup-YYYY-MM-DD.tar.sha256`.
The tar contains three gpg-encrypted members:

| Member | Content |
|---|---|
| `surrealdb.surql.zst.gpg` | Logical `surreal export` of the Core namespace/database (one consistent read transaction) |
| `etc-jarvis.tar.zst.gpg` | `/etc/jarvis` including `secrets/`, with owner/group names and modes |
| `manifest.json.gpg` | Format version, time, SurrealDB image digest, per-table counts (live before, live after, restored), export and member hashes |

Members are encrypted to the owner's **public** keys. The Home Node holds no
private key, so it cannot decrypt its own backups.

Not included, by design:
- Signing keys (updater, Android, Apple). Keep separate offline copies.
- Core release binaries. Reinstall verified releases instead of restoring binaries.
- `/var/lib/jarvis/agents` and worker state. Both are reconstructable.
- Data of other services on this machine.

## What a run checks

1. It counts rows per table in the live database, exports, and counts again.
2. It imports the export into a disposable, networkless SurrealDB container
   (same pinned image, in-memory, 1 GiB limit). It requires the same tables,
   and each restored count must lie between the two live counts. The live
   database is never written.
3. It encrypts, writes the archive atomically, verifies the checksum and
   member layout, and only then prunes older archives (7 daily, 4 weekly).
   The archive it just published is never removed. While any archive carries
   a later date (clock skew or a manual copy), nothing is pruned and the run
   logs a warning.

Before it starts, the run refuses to continue unless the destination has
room for twice the newest archive plus 1 GiB, and `/run` has 2 GiB for the
plaintext export.

If any step fails before publishing, nothing is published, the previous
archive is untouched, and the temporary files are removed. A second run on the same day
replaces that day's archive and checksum, each with an atomic rename. The
plaintext export, capped at 1 GiB, exists only on tmpfs below `/run` during
the run. tmpfs can be swapped to disk.

The run holds the Core updater lock and waits up to 10 minutes for a running
update.

## Local-only backup

Archives stay in `/var/backups/jarvis-dr` on the Home Node, encrypted to a key
that is generated on the host once and then kept only by the owner.

**Protects against:** a bad migration or Core bug that damages data, database
corruption, and operator error (deleted records, a broken `/etc/jarvis`), up
to 3 to 4 weeks back.

**Does not protect against:** loss or failure of the SSD, theft of the
machine, fire, or a compromised host (root can delete the archives, and an
attacker already has the live data). For those you need a copy off the
machine.

**Key trade-off.** `init-local` creates the key pair in a throwaway keyring
on tmpfs, prints the private key once, and keeps only the public key. The
private key therefore exists on this host for a few seconds and in your
terminal scrollback, and it has no passphrase: whoever holds the printed
block can read every archive, including the secrets from `/etc/jarvis`. If
you lose it, every archive is unreadable. The [Mac setup](#one-time-setup-with-owner-keys-on-a-mac)
below never puts a private key on the host and remains the stronger option.

Archives are encrypted, not signed. Anyone with the public key, such as root
on this host, can build an archive that passes `verify --decrypt`. A
compromised host is out of scope for a local-only backup anyway.

### Set up, once

Needs a Core release that ships this version of `jarvis-backup`.

```bash
# Only if the command is missing (Home Nodes prepared before it shipped):
sudo ln -sfn /opt/jarvis/current/jarvis-backup /usr/local/sbin/jarvis-backup
sudo jarvis-backup init-local
```

`init-local` creates `/var/backups/jarvis-dr` (root, 0700), writes
`/etc/jarvis-backup/backup.conf` and `recipients.asc`, and prints the private
key. Copy the whole block, including the `BEGIN` and `END` lines, into your
password manager (a paper copy as well is wise), then run `clear`. It refuses
to run when either file already exists.

### First run and decrypt check

```bash
sudo systemctl start jarvis-backup.service
journalctl -u jarvis-backup --since today
sudo jarvis-backup verify --decrypt
```

Paste the private key and press Ctrl-D. The command decrypts every member of
the newest archive on tmpfs, checks it against the manifest, and removes the
plaintext and the key again. The pasted key shows on screen, so run `clear`
afterwards. Do not leave the prompt open: it holds the backup lock, and a
scheduled run meanwhile exits with code 75. Do this now, to prove the saved key works, and
repeat it every month. `verify --decrypt` and `restore` take a key without a
passphrase, such as the `init-local` key; for Mac keys use the
[decrypt drill on the Mac](#quarterly-decrypt-drill-on-the-mac).

### Daily timer

```bash
sudo systemctl enable --now jarvis-backup.timer
systemctl list-timers jarvis-backup.timer
```

Core keeps running: the database is read with SurrealDB's online export (one
read transaction), the row counts are compared with the live database, and
the export is test-imported into a disposable container before it is
encrypted. See [Daily schedule](#daily-schedule) for retries.

### Restore into a scratch directory

```bash
sudo jarvis-backup restore /var/backups/jarvis-dr/jarvis-backup-YYYY-MM-DD.tar /run/jarvis-restore
```

Paste the private key and press Ctrl-D. This creates `/run/jarvis-restore`
(0700, in RAM) with `export.surql`, `etc-jarvis.tar` and `manifest.json`,
after the same checks as `verify --decrypt`. Live data is not touched. The
target must not exist yet. Its parent must be root-owned, not writable by
others and on the same filesystem as `/run`, so the plaintext is only renamed,
never copied to disk. Inspect it:

```bash
sudo jq '.created_at, .tables' /run/jarvis-restore/manifest.json
sudo tar -tvf /run/jarvis-restore/etc-jarvis.tar
```

It holds plaintext secrets: run `sudo rm -r /run/jarvis-restore` when done.

### Roll the database back to an archive

Core is down while you do this.

1. Restore into `/run/jarvis-restore` as above. Then check that the archive
   was made with the SurrealDB image that runs now: the two digests must be
   equal, because an export from another major version does not import.
   ```bash
   sudo jq -r .surrealdb_image /run/jarvis-restore/manifest.json
   sudo grep '^SURREALDB_IMAGE=' /etc/jarvis/surrealdb.env
   ```
   The export also holds the Core database user. If the Core database
   password was rotated after the archive was made, restore `/etc/jarvis`
   from the same archive as well (step 3 of disaster recovery), or Core
   cannot log in.
2. Stop scheduled backups, Core and the database, and keep the current tree:
   ```bash
   sudo systemctl stop jarvis-backup.timer jarvis-backup.service jarvis-core.service jarvis-surrealdb.service
   sudo docker compose --env-file /etc/jarvis/surrealdb.env -f /opt/jarvis/surrealdb/docker-compose.yml ps --status running -q
   sudo mv /var/lib/jarvis/surrealdb /var/lib/jarvis/surrealdb.before-restore
   ```
   The `ps` command must print nothing.
3. Follow steps 4 to 6 of [Disaster recovery](#disaster-recovery-onto-a-fresh-home-node),
   with `/run/jarvis-restore/export.surql` in place of
   `/root/restore/export.surql`. In step 4, use
   `sudo systemctl start jarvis-surrealdb.service` instead of the start
   helper. It starts SurrealDB on a new, empty data directory. Restore
   `/etc/jarvis` (step 3) only when it is damaged or the password changed,
   and then from the same archive.
4. `sudo systemctl start jarvis-backup.timer`, and delete `/run/jarvis-restore`.

To undo the rollback:

```bash
sudo systemctl stop jarvis-core.service jarvis-surrealdb.service
sudo rm -r /var/lib/jarvis/surrealdb
sudo mv /var/lib/jarvis/surrealdb.before-restore /var/lib/jarvis/surrealdb
sudo systemctl start jarvis-surrealdb.service jarvis-core.service
```

Delete `/var/lib/jarvis/surrealdb.before-restore` once Core works.

### Turning it off

```bash
sudo systemctl disable --now jarvis-backup.timer
```

Archives and configuration stay. To remove them as well (this deletes every
archive):

```bash
sudo rm /etc/jarvis-backup/backup.conf /etc/jarvis-backup/recipients.asc
sudo rm -r /var/backups/jarvis-dr
```

Releases older than this version keep only one archive: after a Core
rollback, the first run deletes all older archives. Disable the timer before
rolling back if you need that history.

## One-time setup with owner keys on a Mac

### 1. Owner keys, on the Mac

```bash
brew install gnupg
gpg --quick-gen-key 'Jarvis backup <you@example.com>' default default never
gpg --quick-gen-key 'Jarvis backup recovery <you@example.com>' default default never
gpg --list-keys --with-colons | awk -F: '$1 == "fpr" { print $10 }'
gpg --armor --export <MAC_FPR> <RECOVERY_FPR> > jarvis-backup-recipients.asc
```

Use a strong passphrase for both keys. Move the recovery private key off the
Mac: export it (`gpg --armor --export-secret-keys <RECOVERY_FPR>`), store it
offline (paper or a hardware token), and delete it from the Mac.
**If every private key is lost, every archive is unreadable.**

### 2. Host configuration, on the Home Node

```bash
sudo install -d -o root -g root -m 0700 /etc/jarvis-backup /var/backups/jarvis-dr
sudo install -o root -g root -m 0600 jarvis-backup-recipients.asc /etc/jarvis-backup/recipients.asc
sudo install -o root -g root -m 0600 /dev/stdin /etc/jarvis-backup/backup.conf <<'EOF'
destination=/var/backups/jarvis-dr
recipients=<MAC_FPR>,<RECOVERY_FPR>
EOF
```

`recipients.asc` must contain exactly the configured public keys and no
private key. The run refuses anything else.

## Running a backup

`jarvis-backup` ships inside every verified Core release as
`/opt/jarvis/current/jarvis-backup`. `prepare-home-node.sh` links
`/usr/local/sbin/jarvis-backup` to it. On a Home Node that was prepared
before this, create the link once:

```bash
sudo ln -sfn /opt/jarvis/current/jarvis-backup /usr/local/sbin/jarvis-backup
```

```bash
sudo jarvis-backup create
sudo jarvis-backup verify      # newest archive; or pass an archive path
```

`verify` checks the checksum and the member layout and needs no key.
`verify --decrypt` and `restore` also decrypt, see
[Local-only backup](#local-only-backup).

After a rollback to a release older than the one that introduced it, the
command is absent until the next update; existing archives are unaffected.

Exit code 75 means another backup, a Core update or a configuration change
was running. Retry later.

## Daily schedule

Core releases ship `jarvis-backup.timer`, **disabled**. Installers and updates
never enable it. Finish the one-time setup first: without
`/etc/jarvis-backup/backup.conf` every run fails and points to this document.
The destination must not be below `/usr`, `/boot`, `/etc`, `/home` or `/root`; the
service sees those read-only.

Test one run, then enable the timer:

```bash
sudo systemctl start jarvis-backup.service
journalctl -u jarvis-backup --since today
sudo systemctl enable --now jarvis-backup.timer
systemctl list-timers jarvis-backup.timer
```

To stop scheduled backups:

```bash
sudo systemctl disable --now jarvis-backup.timer
```

The timer runs daily at 06:00 local time, plus a random delay of up to
15 minutes. A run missed while the machine was off runs at the next boot.
The schedule is fixed: a drop-in that changes `OnCalendar`, `Persistent` or
`RandomizedDelaySec` blocks Core updates until it is removed.

A failed run is retried after 10 minutes, at most 3 starts within 2 hours.
This covers a boot catch-up that starts before Docker or the database is
ready, and a run that finds another backup, a Core update or a configuration
change in progress (exit code 75). Each failed attempt shows the service as
failed; the previous archive is always kept. If every attempt fails, the
timer tries again the next morning.

While a backup runs it holds the Core updater lock. A 5-minute
`jarvis-updater.service` run that starts meanwhile exits with code 75
("another update is running") and shows as failed. That is harmless: the
next updater run retries.

**Disable the timer before rolling back** to a Core release without scheduled
backups. The updater refuses that rollback while the timer is enabled or
active, or a backup is running.

## Copying off the machine

```bash
# On the Home Node: hand one archive to your user account.
sudo install -o "$USER" -m 0600 /var/backups/jarvis-dr/jarvis-backup-YYYY-MM-DD.tar{,.sha256} ~/
# On the Mac:
scp 'homenode:jarvis-backup-YYYY-MM-DD.tar*' ~/JarvisBackups/
cd ~/JarvisBackups && shasum -a 256 -c jarvis-backup-YYYY-MM-DD.tar.sha256
# Sign the copy you verified. The public recipient keys let anyone build a
# valid-looking archive and checksum; only your signature proves this one is yours.
gpg --local-user <MAC_FPR> --detach-sign jarvis-backup-YYYY-MM-DD.tar
# Back on the Home Node:
rm ~/jarvis-backup-YYYY-MM-DD.tar ~/jarvis-backup-YYYY-MM-DD.tar.sha256
```

## Quarterly decrypt drill, on the Mac

This proves the full chain without the key ever touching the Home Node.

```bash
mkdir drill && tar -C drill -xf jarvis-backup-YYYY-MM-DD.tar
gpg -d drill/manifest.json.gpg | jq '.export_sha256, .tables'
gpg -d drill/surrealdb.surql.zst.gpg | zstd -d | shasum -a 256
```

The last hash must equal `export_sha256` in the manifest. Repeat the drill
once with the recovery key. Delete `drill/` afterwards.

## Disaster recovery onto a fresh Home Node

Restore the database and `/etc/jarvis` from the **same** archive: the Core
database password in `core.env` pairs with the `core` user inside the export.

1. Install Ubuntu, then a verified Core release with the normal setup.
   Stop before the SurrealDB initialisation step. Never restore binaries from
   a backup.
2. On the Mac, check your signature, then decrypt the two data members into a
   private directory:
   ```bash
   gpg --verify jarvis-backup-YYYY-MM-DD.tar.sig jarvis-backup-YYYY-MM-DD.tar
   umask 077 && mkdir restore
   tar -C restore -xf jarvis-backup-YYYY-MM-DD.tar
   gpg -d restore/etc-jarvis.tar.zst.gpg | zstd -d > restore/etc-jarvis.tar
   gpg -d restore/surrealdb.surql.zst.gpg | zstd -d > restore/export.surql
   ```
   Copy both to the new host into a 0700 directory, for example
   `/root/restore`, and delete the plaintext copies on the Mac. Plaintext on an
   SSD is not reliably erased by `rm`; prefer an encrypted volume on the Mac
   and a host with full-disk encryption.
3. Restore `/etc/jarvis`. Only regular files and directories under
   `etc/jarvis/` are accepted. Extract as root so owners map by name:
   ```bash
   sudo tar -tvf /root/restore/etc-jarvis.tar | awk '$1 !~ /^[-d]/ { bad = 1 } END { exit bad }' \
     && [ -z "$(sudo tar -tf /root/restore/etc-jarvis.tar | grep -v '^etc/jarvis\(/\|$\)')" ] \
     && sudo tar -C / -xpf /root/restore/etc-jarvis.tar \
     || echo 'STOP: unexpected entry or extraction error'
   ```
4. Start SurrealDB with the restored `surrealdb.env`
   (`sudo /usr/local/libexec/jarvis/start-production-surrealdb`). Keep Core
   stopped. Confirm that the database is empty: `INFO FOR DB` must list no
   tables, or report that the database does not exist yet.
   **Never import into a non-empty database.**
5. Import:
   The image has no shell, so place the export in the root-only data
   directory, which the container mounts as `/data`:
   ```bash
   sudo install -o root -g root -m 0600 /root/restore/export.surql /var/lib/jarvis/surrealdb/restore-export.surql
   sudo docker compose --env-file /etc/jarvis/surrealdb.env -f /opt/jarvis/surrealdb/docker-compose.yml \
       exec -T surrealdb /surreal import --endpoint http://127.0.0.1:8000 \
       --namespace jarvis --database core /data/restore-export.surql
   sudo rm /var/lib/jarvis/surrealdb/restore-export.surql
   ```
   Use the namespace and database from `/etc/jarvis/surrealdb-core-provisioned`.
6. Start Core and run `verify-home-node`. Check owners and modes under
   `/etc/jarvis`, the service identities and the listeners before enabling
   public ingress. Delete `/root/restore`.

### After a suspected compromise

Do not trust restored credentials. Before re-enabling ingress, rotate provider
API keys, the SurrealDB root and Core passwords, and SSH/deploy keys and
private repository tokens, and revoke every device and session. An older
database can also bring back devices and sessions that were revoked after
the backup was taken; revoke those again.
