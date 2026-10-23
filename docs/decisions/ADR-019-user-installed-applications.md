# ADR-019: User-installed applications

- **Status:** Accepted; not implemented (tracked by #133–#138, epic #139)
- **Date:** 2026-10-23
- **Supersedes:** none
- **Extends:** [ADR-013](ADR-013-application-packaging.md) (packaging),
  [ADR-017](ADR-017-linux-app-runtime.md) (AppImage import),
  [ADR-018](ADR-018-networking.md) (who may declare `network=client`)
- **Related:** [ADR-006](ADR-006-application-isolation.md),
  [ADR-014](ADR-014-signed-updates.md),
  [threat model](../security/threat-model.md)

## Context

Today an AppImage runs only if the build stages it beside a signed `AETHERAI`
import record and the record's publisher is in the kernel's build-time
`TrustStore` (`kernel/src/linux.rs`, `import_app`). A user who downloads an
AppImage cannot install it. Upstream AppImages carry no Aether record, and
most carry no signature.

The owner has decided the user model, following macOS:

1. Opening a downloaded `.AppImage` **mounts** it like a `.dmg`. A window
   shows the app tile next to an Applications folder. Nothing in the image
   runs.
2. **Dragging the app onto Applications installs it.** The app is copied into
   a `<Name>.app` bundle that holds the actual files. There is no installer
   wizard and no store.
3. The **first launch needs approval**, like Gatekeeper.
4. Apps **update themselves safely**: AppImage update information, zsync
   deltas, key or source pinning, and an atomic swap with rollback.

This ADR records the decisions that the step issues build on. It does not
weaken ADR-006: installing an app grants it nothing beyond what a built-in
app gets.

## Decision

### 1. Bundle layout

An installed app is a directory `/system/applications/<Name>.app/`:

```text
<Name>.app/
  Contents/
    <Name>.AppImage      the downloaded file, byte for byte
    Info                 signed install record (section 3)
    Resources/icon.png   extracted .DirIcon
    Previous/            the prior version: <Name>.AppImage and its Info
    Pending              signed update journal, present only mid-update
```

- The AppImage is stored **verbatim**. This keeps its SHA-256 and any embedded
  signature, and lets updates use zsync against the installed bytes. It is
  named after the bundle, not the download, so the path is stable across
  updates. The download's file name is kept in `Info` as `source`.
- `<Name>` comes from the `.desktop` `Name`, reduced to a safe file name.
- `Resources/icon.png` is copied only if `.DirIcon` resolves (following at
  most one symlink inside the image) to a PNG of at most 1 MiB and 512×512.
  Otherwise the generic app icon is used. Its digest is in `Info`, so a
  swapped icon (spoofing another app) counts as tampering.
- `Previous/` holds exactly one prior version. It is used only for rollback.
- Files shows a `.app` as one item; "Show Package Contents" opens it
  read-only.

### 2. Location and who may write it

- **One system-wide location, `/system/applications`.** There is no
  `~/Applications`. Aether is a single-user machine today, Files already shows
  this folder as "Applications", and one location means one launcher list and
  one set of update checks. A per-user location is a later decision that
  arrives with multiple user accounts.
- **Only the install service writes there.** The install service is part of
  the kernel today (a `linuxd` operation once the Linux ABI moves to ring 3).
  It accepts requests only from the trusted desktop (Files, Settings, Get
  Info) and the App Updates service. No application domain, Terminal program,
  or portal grant can reach `/system/applications`. Files does not copy into
  it as a file copy. A drop there is an install request.
- Built-in apps from the system image are listed in the same folder, are
  read-only, and cannot be trashed.

### 3. The per-machine install key and the `Info` record

- **Generation.** When the kernel boots and finds no install key, it creates
  an Ed25519 key pair from the kernel DRBG (`kernel/src/random.rs`, seeded
  from `RDSEED`/`RDRAND`). If only the TSC fallback is available, it does not
  create a key and installing is disabled with an explanation.
- **Where it is held.** The private key stays in kernel memory. It is saved
  at `/system/security/install-key`, which no process root, portal grant, or
  Files location can reach. Until durable storage exists, the RAM disk loses
  the key and all bundles at power-off. Once storage is durable, the key file
  is sealed to the TPM under verified boot (#98). Security Center shows the
  public-key fingerprint.
- **What it signs.** Only install records: `Info`, `Previous/Info`, and
  `Pending`. It never signs code, and no interface lets an application ask
  for a signature.
- **Record format.** `Info` uses the same framing as the import record
  (`apps/linux/appimage/src/manifest.rs`), with magic `AETHERIN` and format
  version 1. The body is canonical `key=value` lines, at most 2 KiB. Unknown
  or duplicated keys fail closed, and any change to the fields bumps the
  version.

  | Field | Meaning |
  |---|---|
  | `id` | App id (below). Fixed for the life of the install. |
  | `name`, `version` | From `.desktop` / AppStream metadata. |
  | `image`, `sha256`, `size` | The stored AppImage and its digest. |
  | `icon-sha256` | Digest of `Resources/icon.png`, or `none`. |
  | `source`, `source-url` | Download file name; URL if the quarantine attribute has one. |
  | `installed`, `updated` | Timestamps. |
  | `signer` | `openpgp:<fingerprint>` (section 5) or `none`. |
  | `update-info` | Raw `.upd_info`, or `none` ("Updates: manual"). |
  | `update-source` | Canonical pinned source derived from `update-info`, or `none`. |
  | `runtime`, `profile` | `linux-appimage`; the Gate 3 glibc profile by default. |
  | `network` | `client`. This makes the app eligible to ask; it is not consent. |
  | `memory` | `jit` only after the user approves it (section 4). |
  | `quarantine` | `pending` or `approved:<timestamp>`. |
  | `auto-update` | `on` or `paused` (paused after a rollback). |

- **App id.** The id comes from the AppStream `<id>` if that is a valid
  reverse-domain name, then from the `.desktop` file's base name if that is
  one, and otherwise is `appimage.<name>`. Ids owned by built-in apps are
  refused. "Keep Both" gives the second bundle the id `<id>.2`, so the two
  copies get separate private storage.
- **The trust store gains a second, separate publisher.** The machine key is
  added as `local-install`. It is accepted only for `AETHERIN` records, and
  the build-time publisher only for `AETHERAI` records. Neither can stand in
  for the other.
- **Checks at every launch, in order.** The `Info` framing and signature
  verify against the machine key. The bundle name and `image` match. The
  SHA-256 of `Contents/<Name>.AppImage` equals `sha256`, and the icon digest
  matches. `quarantine` is `approved`. Only then does the existing
  `aether-appimage`/`aether-squashfs` import run. The whole image is hashed on
  every launch (about 0.5 s for 100 MB). Caching the digest needs a
  filesystem generation counter that apps cannot forge, and that is later
  work.
- **If a check fails,** the app does not launch. Files shows it as
  "damaged or modified" and offers to reinstall. A bundle signed by an
  unknown key (lost or regenerated machine key) is treated as a fresh
  download: it must be installed again and approved again. Its private data
  is kept, because private data is keyed by id.

### 4. Quarantine and first-launch approval

- Every file that an application domain creates gets a provenance attribute
  (the writing app's id and, if known, the source URL), and system copies keep
  it. This attribute is **for information only**: the install service
  quarantines every user install whether or not the attribute is present.
  On macOS a missing attribute skips Gatekeeper; here it cannot.
- The first launch of a quarantined app opens a dialog drawn by the kernel
  desktop. It shows the name and icon, the signer ("Signed by ‹uid›",
  fingerprint) or **"unidentified developer"**, the SHA-256, the source, and
  what the app may ask for later: network (asked separately on first use),
  files (only through the portal), and JIT. The Open button ignores clicks
  for the first 500 ms, as the ADR-018 prompt does.
  - Signed app: **Open** / **Cancel**.
  - Unsigned app: **Cancel** only. The user then approves it with
    **Open Anyway** in Settings → Privacy & Security, available for one hour
    after the blocked attempt.
- **JIT is a separate opt-in in the same dialog** and is off by default.
  W+X mappings stay refused (ADR-017) unless the user turns it on, and Get
  Info can turn it off again.
- Approval rewrites `Info` with `quarantine=approved:<time>` and is recorded
  only there. The record is signed, so a tampered bundle cannot carry an
  approval over.

### 5. Embedded AppImage signatures

AppImages can embed an OpenPGP signature (`.sha256_sig`) and the public key
that made it (`.sig_key`). The key travels inside the file. On first install
it proves only that the file is consistent with a key it brought with it.

- The install service verifies the signature with a bounded, host-tested,
  `no_std` OpenPGP v4 verifier: Ed25519 and RSA 2048–4096, with fixed limits.
  Until that verifier exists, **every app is "unidentified developer"**.
  Aether shows "signed" only after it has verified the signature.
- A signature that is present but invalid **blocks the install**. It means the
  file was modified after signing.
- A valid signature is shown as "Signed by ‹uid› (fingerprint …)". It is
  trust on first use, and the dialog says that Aether does not know this
  developer. Its value is mainly for updates: the fingerprint is pinned in
  `signer` and must sign every update.
- AppImages built by Aether's own pipeline keep using the build-time
  `AETHERAI` path and are built-in apps, not user installs.

### 6. Update trust

The App Updates service (#138) asks once for network consent for itself, not
once per app. It reads `update-info` (`zsync|<url>` or
`gh-releases-zsync|…`), downloads a zsync delta or the full file, and
computes the SHA-256 itself. The SHA-1 that zsync carries is not trusted.

An update installs without asking only if all of these hold:

- the new image has the same `id`;
- its version is higher (a downgrade is only ever an explicit rollback);
- for a **signed** app, it carries a valid signature from the **pinned
  fingerprint**;
- for an **unsigned** app, it came over HTTPS with certificate verification
  from the **pinned `update-source`**, and its own `.upd_info` names the same
  source. An app cannot move its update source by itself. Until the service
  has verified TLS, unsigned apps get "update available" notifications only.

Any other case (a new key, a signed app turning unsigned, a new source, or a
manual Replace from a mounted image whose signer differs) needs explicit
approval in a dialog that shows the change. Afterwards the app goes back into
quarantine for one more first-launch approval. An update that passes the pins
keeps the existing approval, JIT setting, and grants.

### 7. Atomic install, update, and rollback

- **Install:** the bundle is built completely in
  `/system/applications/.staging/<random>/`. After the image is validated and
  `Info` is written last, one directory rename gives it its final
  `<Name>.app` name. A failure or power loss leaves only staging, which is
  deleted at the next boot. Nothing is ever half-installed.
- **Update or Replace:**
  1. Write the new image to `Contents/.staging` and check its digest.
  2. Write a signed `Pending` record that names the old and new digests.
  3. Copy the current image and `Info` to `Previous/`. `Previous/Info` is
     written last.
  4. Rename the staged image over `Contents/<Name>.AppImage`. This is the
     atomic file replacement.
  5. Rename the new `Info` into place, then delete `Pending`.

  A running app is asked to quit first, and an update never replaces an
  image that is in use. On boot and on launch, a digest mismatch with a valid
  `Pending` is completed if the image matches the new digest, or restored
  from `Previous/` if it matches neither. A mismatch without a `Pending`
  record is tampering (section 3), never an interrupted update.
- **Rollback** (Get Info): swap the current version and `Previous/` using the
  same journal, so a rollback can itself be undone. Then set
  `auto-update=paused`, so the same update does not immediately come back.
  Rollback is a deliberate downgrade, in the spirit of ADR-014 recovery.

### 8. Uninstall and private data

- Dragging `<Name>.app` to Trash (or Move to Trash) quits the app and moves
  the bundle to `/home/Trash` through the install service, using the existing
  Trash index. A bundle in Trash cannot be launched. Put Back verifies it
  again before restoring it.
- **Empty Trash** asks: "Also delete ‹App›'s data and settings?" The choices
  are **Keep Data** (the default, so nothing is lost by accident) and
  **Delete Data**. Delete Data removes the private storage and overlay for
  the id. Either way, the app's portal grants and network consent are revoked
  when the bundle is deleted.

### 9. Isolation is unchanged

An installed app runs exactly as a built-in Linux app does under ADR-006 and
ADR-017. It gets its own app domain, private storage derived from its id
(not from the bundle path), a read-only image root with a private overlay,
user files only through the portal, and network only after the ADR-018
first-use prompt. `memory=jit` is allowed only with the user's approval.
Installing an app and approving its first launch grant nothing more.

### 10. Threat-model changes

**A malicious AppImage can:** run after the user approves it; use its own
private storage, CPU, and memory within its profile limits; ask for files the
user picks, and for network access the user allows; imitate other apps inside
its own windows (consent prompts are drawn by the kernel desktop and cannot be
answered in the first 500 ms); and reach every Linux syscall handler. While
the ADR-017 ring-0 deviation stands, a kernel bug in one of those handlers is
a full compromise, including the install key. That is the largest residual
risk.

**It cannot:** run code when it is mounted or installed (only the bounded
Rust ELF, SquashFS, `.desktop`, and PNG parsers touch it); write
`/system/applications` or another app's bundle or data; forge or edit an
`Info` record; approve itself or grant itself JIT or network eligibility;
or update another app or move its own update source.

**Offline tampering (until #98):** the boot image and the kernel that holds
the trust keys are not verified. Anyone who can write the disk can replace
the kernel, read the install key while it is not sealed, forge records, or
replay an older signed `Info` together with its older image. There is no
monotonic per-app counter. This ADR protects bundles against applications
and running software, not against an attacker with the disk. Sealing the key
to the TPM under verified boot, and a counter in sealed storage, close the
gap. Until then the threat model lists this as a known gap.

## Consequences

- New code: an `AETHERIN` record codec and an install service, an OpenPGP v4
  verifier, a `.upd_info` parser, a zsync client, and a per-file provenance
  attribute in the filesystem. Each is bounded and host-tested.
- Installing depends on durable storage to be useful past one power cycle.
  Until then the key and bundles live in the RAM disk, and tests reinstall on
  each boot.
- Hashing on every launch costs time for large apps such as Firefox (#103).
  This is accepted until a verified digest cache exists.
- Machine-key signatures mean an app installed on one machine cannot simply
  be copied to another. The other machine treats it as a new download.

## Alternatives considered

- **`~/Applications` per user, or both locations:** rejected for now. There
  is one user, and two locations would double the launcher and update logic.
- **An unsigned registry database in `/system` instead of signed `Info`
  files:** rejected. It is a second source of truth, it does not travel with
  the bundle through Trash, and it detects tampering no better.
- **HMAC with a symmetric machine key:** rejected. Ed25519 reuses the
  existing `TrustStore` and verifier, and a public fingerprint can be shown
  and checked.
- **Unpacking the SquashFS into the bundle at install:** rejected. It loses
  the original digest and signature, multiplies the number of files, and
  breaks zsync deltas.
- **Running the AppImage's own runtime or FUSE to mount it:** rejected, as in
  ADR-017. Mounting is a read-only parse that executes nothing.
- **Quarantine only when the attribute is present:** rejected. A copy that
  loses the attribute would skip approval.
- **Treating embedded signatures as publisher identity:** rejected. The key
  is self-supplied, so it is pinned for updates and is not proof of who the
  developer is.
- **Silent updates from any URL in the new image's `.upd_info`:** rejected.
  It lets a compromised release redirect all later updates.
