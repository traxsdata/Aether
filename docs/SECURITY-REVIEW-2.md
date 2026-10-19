# Adversarial review of æther, round 2 (October 2026)

A second, deeper pass over `main` after round 1's seven fixes were merged
(PR #7). The goal set by the first review stands: an untrusted app (a Craft
app, the browser, any installed package), a hostile network, or a hostile
disk must not be able to touch the kernel, other apps, or data they weren't
given, and DLP must hold.

Round 1 read the code once looking for first-order mistakes. This round went
back over the hard surfaces — the syscall boundary, the Linux ABI shim, the
TCP/TLS/X.509/HTTP stack the browser points at hostile servers, the encrypted
file system, DLP egress, and the window manager — trying to find a *second*
mistake behind each defence: a bound that is right in the common case but
wrong at the edge, a value trusted because "it can't happen", a check that
holds only because another check upstream happens to hold.

One such latent bug was found and fixed; everything else held. For a review
whose job is to break the thing, "it held" is the headline, so the surfaces
that were pushed on and did not give are written up as carefully as the fix.

Severity is for æther's own threat model, same as round 1.

## Fixed

| # | Severity | Weakness | Fix |
|---|----------|----------|-----|
| 1 | Low (latent) | `afs_dir_entry()` copied a folder record's name with `memcpy(name, r.name, r.len)` using the on-disk length byte `r.len` (0–255) directly. `r.name` is 58 bytes (`NAME_MAX`) and every caller's `name` buffer is `NAME_MAX+1`, so an `r.len > 58` would over-read the record and overflow the caller's stack buffer by up to ~195 bytes, then write a NUL past it. | `kernel/afs.c` clamps `r.len` to `NAME_MAX` before the copy; `kernel/ktest.c` adds a round-trip of a full-length (58-char) name so the clamp can't silently truncate a legitimate name. |

### Why it is Low, not High

The overflow is real memory-unsafety, but it is not reachable in the current
build. A folder record only ever reaches `afs_dir_entry` through
`afs_read`, which opens a ChaCha20-Poly1305-sealed block whose tag is bound by
AAD to its kind, inode, generation and index (`read_sealed`,
`struct aad`). The one writer, `afs_dir_add`, sets
`r.len = strlen(name)` only after `name_ok()` has capped the name at
`NAME_MAX`. So to land an `r.len > 58` on disk an attacker needs either the
volume key — in which case the machine is already theirs — or a forged AEAD
tag, which the sealing is there to prevent (and which round 1's `files_tamper`
/ `files_swap` tests confirm fails closed). The clamp is belt-and-suspenders:
it removes the only place where a single corrupt or future-mis-written
metadata byte would turn into a kernel stack overflow, at the cost of one
comparison. There was no way to write a test that drives the overflow without
also handing the test the volume key, which is precisely why the finding is
Low rather than High — so the regression test instead pins the boundary the
fix must not break (a 58-byte name must still round-trip intact).

## Still open (confirmed, not regressions)

Round 1's roadmap items were re-examined against the code this round and all
still stand. Two are worth restating with the exact consequence, because the
sealing's strength makes it easy to overestimate what it covers.

- **Package authenticity is still integrity-only.** `app_check` hashes the
  ELF and compares it to `h->elf_sha256`, but that digest lives in the
  package's own header, so it catches a damaged download, not a deliberately
  rebuilt one. Firewall "Always allow" rules are keyed by app *name*
  (round 1), so a replaced package of the same name inherits them. The
  containment that makes this survivable is real and was re-verified:
  `programs_install` forces every installed package to `TRUST_UNTRUSTED` and
  masks its caps to `CAP_WINDOW|CAP_PICK|CAP_STORAGE|CAP_NET`
  (`kernel/apps.c`), and `program_verify` re-hashes on first run. So a tampered
  package is still a sandboxed untrusted app — it cannot gain privilege, only
  impersonate another untrusted app's identity to the firewall and the user.
  Signing packages closes it.

- **Disk metadata has no integrity or freshness.** Sealed blocks are bound to
  their position, so an attacker with raw disk access cannot read plaintext,
  forge a block's contents, or move a block elsewhere (verified: the round-1
  `files_swap` test shows a position swap fails `-E_IO`). What is *not*
  protected: the free-space bitmap is written with `raw_write`, unsealed
  (`bitmap_flush` in `kernel/afs.c`), and the whole volume has no version or
  Merkle root. Offline tampering therefore cannot break confidentiality or
  forge data, but it can cause denial of service / corruption: flipping the
  bitmap so an in-use block reads as free lets a later allocation reseal it to
  a new owner, and the original file's read of that block then fails `-E_IO`;
  and the entire volume can be rolled back to an earlier, validly-sealed
  state. A version counter or Merkle root in the (already authenticated)
  superblock, plus sealing the bitmap, would close this.

- **Global slot exhaustion** (64 kernel stacks, 64 TCP connections; per-app 32
  threads / 16 connections) is unchanged: a few greedy apps can keep others
  from starting. An availability annoyance, not a breach. Note the window
  manager is *not* in this category — see below.

## What held up

Each of these was approached as something to break, not to confirm.

- **The syscall boundary.** Pointer arguments are validated against the
  *caller's* page tables with STAC/CLAC windows (`copy_in`/`copy_out`), and
  the kernel runs non-preemptibly (IF=0, yielding only through
  `proc_block`/`schedule`), which closes the TOCTOU where another thread would
  edit the caller's mappings mid-check. The untrusted-app syscall allowlist is
  enforced in the dispatcher, and — the part worth re-checking — the Linux-ABI
  native-call escape hatch (`0x1000 + N`, `kernel/linux.c`) is re-checked
  against the same `syscall_allow` mask, so a Linux app cannot reach a native
  call its manifest forbids.

- **The browser's hostile-server surface.** TLS 1.2/1.3: the X.509 path
  checks the signature algorithm's inner and outer identifiers match
  (`x509.rs`), and `CertificateVerify` binds the leaf key to the handshake
  transcript, so a swapped-in cert can't ride someone else's signature. RSA
  verification rebuilds the PKCS#1 v1.5 encoding and compares it whole, so no
  parser ever walks attacker-chosen padding (`crypto/rsa.rs`), and PSS does
  the masking/trailer checks by the book. HTTP/1.1 response parsing rejects
  Transfer-Encoding + Content-Length together, conflicting Content-Lengths,
  bare CR and obsolete line folding, and bounds headers (64 KiB / 128 headers)
  (`webnet/.../http.rs`). DEFLATE/gzip/zlib all thread an output cap through
  `inflate`, and the page itself is capped at `MAX_PAGE_BYTES` (8 MiB), so a
  decompression bomb is bounded and, worst case, a contained quota-kill of the
  sandboxed browser.

- **The encrypted file system's sealing**, as above: content confidentiality,
  integrity and position-binding all hold; only unauthenticated metadata and
  rollback remain (listed open).

- **DLP egress.** A process's taint rises to the max label it has read, and
  `sys_net_send` re-checks the taint against the destination at send time, so
  a confidential-tainted app cannot launder data out through a connection it
  opened earlier. Folder entry names/sizes are labelled for untrusted apps
  (round 1 fix #3), and the label-lowering dialog re-validates the file
  (fix #4) — both still in place.

- **The window manager's resource use.** Framebuffers are allocated with
  `pmm_alloc_contig` *outside* the per-app memory quota, which initially looked
  like a quota bypass. It is bounded instead: `wm_create_window` caps an app at
  4 live windows and 64 window allocations over its lifetime, and clamps every
  window to the screen size, with a system-wide ceiling of 16 windows. So an
  app's real footprint can exceed its declared quota by a few window buffers,
  but cannot grow without bound — a small accounting gap, not an exhaustion
  vector. Worth charging to the quota eventually for honesty, not for safety.

## Bottom line

After two reviews the kernel's core promises — untrusted apps stay in their
box, the network stack survives hostile peers, the browser survives hostile
servers, and the file system protects data at rest — hold up to deliberate
attack from the code. The remaining exposure is the known, documented class:
no package signing, no disk anti-rollback or metadata integrity, and
system-wide resource slots. None of those is a containment break; all three
are roadmap work, and this round adds nothing new to that list beyond pinning
down exactly how far the on-disk sealing does and does not reach.
