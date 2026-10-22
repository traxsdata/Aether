# Ransomware blast-radius analysis

Status: host-side prototype analysis, 2026-10-22.
Scope: native file syscalls, per-process roots, exact-file portal grants, and
the `aether-services::ux` grant policy. This is not an external review.

## Question

If a malicious native application ran a file-encryption loop, how much user
data could it destroy, and which existing Aether boundary stops it? The goal is
to test the *containment* claims, not to produce a working ransomware sample.

## Method

A local host harness was run against the same host-testable crates the kernel
builds (`aether-fs`, `aether-services`), seeded into an in-memory `RamDisk`.
The harness is intentionally **not committed** and is not part of this
analysis's deliverables; the results below are the observed output, and the
policy conclusions can be reproduced with a small host test. It never opens a
host path and never persists data.

Deliberately omitted, so the experiment cannot be used as ransomware:

- a fixed, printed, reversible XOR transform instead of encryption;
- no key generation, sealing, or destruction (the test proves recovery);
- no persistence, startup hook, network, propagation, or payment channel;
- no real user files, only synthetic canaries in a 128 KiB ramdisk.

A local run is `cd tests/ransomware-sim && cargo run`; because the directory is
excluded from version control, a fresh checkout will not contain it.

## Results

| Scenario | Files visible | User files overwritten | App-owned files overwritten | Outside-root opens blocked |
|---|---:|---:|---:|---:|
| A. Native app, private root only | 2 (own) | 0 | 2 | 6 |
| B. Native app + exact-file portal grants | 3 | 3 | 0 | 8 |
| C. Hypothetical folder grant (`ux` policy) | 10 | 10 | 0 | 0 |

Interpretation:

- **A is the real security property.** A native task rooted at
  `/apps/<id>` cannot name `../`, a sibling vault, `/home`, or the system
  image. `AetherFs::resolve` rejects the path before lookup, so the attacker
  can only destroy its own data.
- **B is bounded by consent, not by the filesystem.** A native app cannot list
  `/home`. It can only guess names and ask the portal; each approved exact file
  becomes a read/write capability. In the run, 3 approvals produced 3 victims
  and 8 denials. The 8-grant table bound is a resource limit, not a security
  control.
- **C is the dangerous policy shape, not current behaviour.** The kernel portal
  only accepts exact-file grants (`kernel/src/portal.rs`). However,
  `aether-services::ux` already models `GrantScope::Folder` with
  `READ|WRITE|DELETE|RENAME|ENUMERATE`. If that scope is ever wired into the
  portal without a separate bulk-consent step, one click exposes the whole
  workspace. The simulator demonstrates that outcome so the boundary is not
  weakened by accident.

Also observed in the prototype: native syscalls are
`open/read/write/close/list/stat` only. There is no native `rename`, `unlink`,
`truncate`, or directory-create outside the process root, so the `.locked`
extension and ransom-note patterns are not reachable from a native app today.
Aether's Linux ABI writes to a private overlay or a portal staging file; it
does not get a general host filesystem.

## Why the current design helps

1. **Kernel-owned process roots** — the task cannot choose its own root, and
   `..`/absolute/sibling paths fail in `resolve`.
2. **Exact-file portal grants** — there is no directory or wildcard grant, so
   selecting one file does not imply its siblings.
3. **Per-open file capabilities** — a path is not a durable authority; a handle
   is a generation-checked capability with explicit rights.
4. **Small native syscall surface** — no rename/unlink/truncate means the
   classic "encrypt then rename" step needs a different, consent-gated path.
5. **Verified-package permissions** — `portal.read` and `portal.write` are
   separate manifest declarations, and the portal gate fails closed without a
   profile (`kernel/src/fs.rs::portal_permission`).

## Implemented in this slice

The first transparent mitigation is in place: `AetherFs` retains the previous
block chain of an overwritten file as a pre-image instead of freeing it
immediately. `restore_previous` swaps the file with that version, and the
console exposes it as `restore <path>`. Block pressure reclaims pre-images
before a legitimate write or directory allocation fails, so normal use is
unchanged; the retained version is a recovery aid, not live file data.

Limits: one generation per inode, memory-only on the volatile ramdisk, and no
desktop recovery flow yet. The pre-image is also not authenticated or
persisted, so it does not survive reboot and is not a substitute for a durable
version log.

## Gaps that still matter

### P0 — overwrite recovery is partial

The old block chain is now retained for one generation, but the table is
in-memory and a second overwrite (or a multi-chunk write after an `O_TRUNC`)
can displace it. `kernel/src/recovery.rs` still only selects system-image
markers and does not expose user-data versions. There is no retention window,
quota policy, or user-facing restore in Files or Recovery.

Consequences: consent to *write* still means an app can destroy the current
contents; recovery only works while the pre-image survives and the machine has
not rebooted.

### P0 — write grants are persistent with no lifetime

`PortalGrant` stores only `root`, `path`, and `rights`
(`kernel/services/src/portal_store.rs`). There is no expiry, use count, or
one-shot flag, even though `aether-services::ux` already has
`GrantLifetime::{OneTime, Persistent}`. An app approved once can reuse the
grant indefinitely, including after an update.

### P0 — `WRITE` conflates create and replace

`FileRights::WRITE` allows opening an existing file and overwriting its
contents. There is no distinct "replace existing content" right, so the consent
prompt cannot distinguish "save a new document" from "destroy this document".
Ransomware needs only this one right.

### P1 — no bulk-write rate limit or containment response

`sys_write` and `grant_file_open` count opens and denials, but nothing acts on
a burst of writes across many distinct files. The supervisor can quarantine a
failing service, but file-write behaviour is not an input to that decision.
Coupled with persistent grants, a sleeper app can wait and then rewrite its
approved set quickly.

### P1 — grant and audit state are plaintext and unauthenticated

`/system/portal/grants` is parsed as text (`portal_store::parse`). In the RAM
disk this is out of reach of app roots, but when durable storage lands, an
offline or kernel-adjacent attacker could inject or roll back grant records.
Revocation is only trustworthy if the store is authenticated and monotonic.

### P1 — consent fatigue

`portal::request` refuses duplicate pending requests and has a four-entry
pending queue, but there is no per-app request budget, cooldown, or session
summary. A user who approves several files has no single place that says
"this app now holds write access to N files".

### P2 — folder grants exist in policy

`GrantTable::grant` accepts `GrantScope::Folder` with `ENUMERATE|WRITE|DELETE|
RENAME`. The kernel portal does not use that table today, but the policy is a
latent foot-gun. A file grant correctly rejects `ENUMERATE`; folder grants have
no equivalent restriction.

### P2 — no filesystem journal or consistency check

Whole-file replacement is ordered, but directory updates, unlink, and rename
are not journaled. A crash during metadata work, or a future multi-task
filesystem, can leave corruption independent of ransomware. There is no fsck.

## Recommended changes

Prioritised for the next storage/security slice. Each item names an acceptance
test so it can be reviewed as a boundary, not a feature.

### 1. Retain pre-images for user-data writes (P0) — prototype implemented

The old block chain is retained instead of being freed immediately, with a
bounded one-generation table and quota-aware reclamation. The restore path is
`AetherFs::restore_previous` plus the `restore <path>` console command. Tests
cover overwrite, truncate-then-write, rename, delete, stale handles, and
reclamation under disk pressure.

Remaining acceptance work: retain more than one generation or a time window,
persist the version log with authentication, expose restore in Files/Recovery,
and never collect a version that policy says must be retained.

### 2. Make write grants one-shot or expiring by default (P0)

Unify `portal_store::PortalGrant` with `ux::GrantLifetime`. Add `expires_at`
and `max_uses`, persist them, and make persistent write access an explicit
choice. A one-shot grant must fail on replay with `ENOGRANT`.

Acceptance: replay after one use, after expiry, and after app update all fail;
revocation still fails closed even if another record matches.

### 3. Split `CREATE` from `REPLACE` (P0)

Add a distinct right for replacing existing content. Require it for
`portal_open(WRITE)` on an existing file, and have the consent UI say
"modify existing content" rather than "write". Creating a new file should not
imply the ability to destroy an existing one.

Acceptance: a create-only grant cannot open an existing file for write; a
replace grant is shown distinctly in the consent record; the simulator's
scenario B fails to overwrite without the new right.

### 4. Never expose folder grants through the portal (P0)

Keep the portal's exact-file contract. If folder grants are needed later,
require a separate bulk-consent flow with a file count and a per-file revoke
list. Add a portal-boundary test that rejects `GrantScope::Folder`, `ENUMERATE`,
`DELETE`, and `RENAME`, not just at the `ux` layer.

Acceptance: a folder resource or a non-`{READ,WRITE}` right set is rejected
before any filesystem open; a regression test names the folder-grant case.

### 5. Add a kernel write-burst limiter and response (P1)

Track bytes and distinct files written per task/root in a sliding window. On
exceeding a threshold, freeze the task, drop its file capabilities, and raise a
user-visible event. The existing supervisor quarantine and portal audit
counters provide the response surface; the limiter belongs in the syscall/file
service, where the app cannot disable it.

Acceptance: a synthetic task that rewrites N files in one window is frozen
before N+1; unrelated tasks continue; the event is visible in the security
view; limits are per manifest and fail closed.

### 6. Use staged saves for native portal writes (P1)

The Linux path already has `*.sctmp` plus commit-by-rename. Extend the same
pattern to native portal writes: write a staging file, preserve the original,
then commit atomically. Direct in-place write should either be disallowed for
existing files or require an explicit replace grant and a retained pre-image.

Acceptance: a crash or denial between staging and commit leaves the original
unchanged; the committed bytes match; an abandoned staging file is reclaimed.

### 7. Authenticate and make grant state append-only (P1)

When storage is durable, MAC the grant/audit records with a kernel-held key,
include a monotonic sequence, and reject rollback or truncation. Keep an audit
trail of grant, use, revoke, and denied-burst events. This prevents an attacker
from erasing the evidence that would trigger revocation.

Acceptance: modified, reordered, replayed, and truncated stores are rejected
at boot; a valid store loads; revocation survives restart.

### 8. Add a safe-mode recovery with file version restore (P1)

Extend recovery beyond image markers: boot without third-party apps, expose a
read-only view plus version restore, and make the recovery path independent of
the normal desktop. This turns a successful overwrite from permanent loss into
a recovery workflow.

Acceptance: recovery boots with no app profiles registered; a file overwritten
by the simulator can be restored from its pre-image; the original image-set
rollback markers still pass.

### 9. Anti-fatigue consent accounting (P1)

Show the publisher, signature identity, exact path, rights, and a live count of
files the app can write. Add a per-app request budget and cooldown, and never
offer "select all" for write grants. Revoking one grant must not require
walking a folder tree.

Acceptance: request N+1 within the cooldown is refused; the UI/report count
matches the kernel grant table; a bulk-approval path does not exist.

### 10. Treat `portal.write` plus high limits as a high-risk package (P2)

Require review for packages declaring write access, re-consent when an update
adds or widens permissions, and keep the publisher pin. Also wire the existing
`AppStorage` quota (`aether-services::ux`) into writes so an app cannot consume
the volume; today the quota policy is host-tested but not enforced by the file
service.

Acceptance: a package update that adds `portal.write` does not inherit old
consent; a write that exceeds the app quota fails with `ENOSPC`; a compromised
publisher key is still bounded by per-app quota and burst response.

### 11. Journal metadata and add fsck (P2)

Journal directory, rename, and unlink transactions; add a bounded consistency
check at mount. This is broader storage work, but ransomware response depends
on a filesystem that can be trusted after a crash or forced termination.

## Not tested here

- Ring-3/QEMU execution, multi-task concurrency, and the Linux ABI's staging
  path (modelled from code, not executed).
- Durable storage, since the prototype volume is a volatile ramdisk.
- Key management, because the simulator intentionally has none.
- A real external attacker, stolen publisher key, or physical access.

The prototype's small volume, 4 KiB file cap, 8 open-file pool, and 8-grant
table bound the blast radius today. Those are resource limits, not defenses;
the recommendations above are what should remain true when the limits grow.
