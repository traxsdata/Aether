# Threat model

## Assets

- Integrity of the kernel and boot chain.
- Confidentiality and integrity of user files.
- Private application data and credentials.
- Capability tables and security policy.
- System availability and recoverability.

## Current scope

Aether is a bootable research prototype. The implemented boundaries are
firmware → loader, loader → kernel, kernel → ring-3 application,
application → application (private roots), application → user data (file
capabilities and the portal), and service-policy supervision. Service bodies
still run as kernel modules; moving them to ring 3 is follow-up work.

### In-scope adversaries

- A malformed or corrupted kernel image embedded in the loader.
- A firmware memory map with invalid, overlapping, unsorted, or excessive
  regions.
- A loader/kernel version or ABI mismatch.
- A crafted `BootInfo` block or memory-region array that reaches the kernel
  through a bug or a future compromised loader.
- A kernel task using a forged handle, stale generation, insufficient rights,
  or a capability for another endpoint.
- A malformed, oversized, or resource-exhausting IPC message or file request.
- A ring-3 task passing a kernel pointer, an out-of-window range, a forged
  handle, or a stale generation to a syscall.
- A task attempting `..` traversal, a sibling application root, or an
  ungranted portal path.
- A malformed framebuffer descriptor or a service that exhausts its restart
  budget.
- Accidental disclosure of secrets through serial diagnostics.
- A build or CI change that silently removes validation or the boot test.

### Out-of-scope for the prototype

- A malicious firmware or physical attacker with unrestricted hardware access.
- DMA attacks; there is no IOMMU configuration yet.
- Side channels, speculative-execution attacks, and microarchitectural
  isolation.
- Supply-chain compromise of the Rust toolchain or host OS.
- A compromised service body, because services still run in the kernel in this
  prototype.
- Windows compatibility runtime attacks, because no runtime exists.
- Persistent-storage attacks, because the volume is a volatile RAM disk.

## Boundaries and controls

| Boundary | Threat | Current control | Status |
|---|---|---|---|
| Firmware → loader | Oversized/overlapping/corrupt kernel ELF | Bounded parser, bounds and overlap checks, entry check, fixed segment limit | Implemented, host-tested |
| Firmware → loader | Memory map larger than loader storage | Fixed 64 KiB map buffer and 256-region array; fail closed | Implemented |
| Loader → kernel | Stale/wrong hand-off | Magic, version, structure size, reserved fields, aligned count, kernel range, entry, region ordering/overlap validation | Implemented, host-tested |
| Loader → kernel | Invalid enum discriminant | Memory kinds stored as `u32` and decoded fallibly | Implemented |
| Boot path | Firmware leaves NX or WP disabled | Kernel sets `IA32_EFER.NXE` and `CR0.WP` explicitly; fails closed if NX is absent | Implemented; QEMU-tested |
| Boot path | Allocator reuses firmware page tables or kernel memory | Reserve kernel/hand-off; release boot services only after CR3 switch; double-free checks | Implemented, host-tested |
| Boot path | A fault during memory setup resets silently | IDT installed before paging; structured vector/RIP/CR2 report; fault self-test | Implemented, QEMU-tested |
| Kernel memory | Kernel text writable or data executable | 4 KiB W^X page permissions, NX enabled, `CR0.WP` enabled | Implemented; fault-injection test pending |
| Kernel task → task | One task reads or writes another task's memory | Per-task PML4 and private frame at the same virtual address; CR3 switch on context switch; boot-time isolation test | Implemented, QEMU-tested |
| Kernel task → object | Forged handle, stale slot, wrong endpoint, or insufficient rights | Generation-checked handles; per-task tables; endpoint identity checks; rights checks | Implemented, host- and QEMU-tested |
| Kernel task → IPC | Oversized message, queue exhaustion, capability forgery, table exhaustion, sender impersonation | 32-byte bound, four-message queues, critical sections, `TRANSFER` check, atomic receive, kernel-supplied sender identity | Implemented, host- and QEMU-tested |
| Kernel → user | User reads or writes kernel memory | CPL3, user-only PML4 window, supervisor-only identity map, syscall range validation | Implemented, QEMU-tested |
| User → kernel | Malformed syscall pointer, length, handle, or generation | Whole-range page validation, generation/type/right checks, bounded copy | Implemented, QEMU-tested |
| User → user | App reads a sibling's private data | Kernel-owned root, `..` rejection, per-task file capabilities | Implemented, QEMU-tested |
| App → files | App opens an ungranted outside file | Exact portal grant keyed by root, path, and rights | Implemented prototype |
| App → kernel | Ring-3 fault takes down the kernel | Fault retires only the faulting task and reschedules | Implemented, QEMU-tested |
| Service → service | Impersonation or capability forgery | Declared profiles, dependency order, restart/quarantine | Policy prototype; bodies still in kernel |
| Diagnostics | Secret leakage | Boot code prints only version, addresses, and memory sizes | Policy; no secrets exist yet |
| Build | Malformed image reaches CI as “passing” | Local `make ci` gate; boot, shell, fault, and negative tests require markers on a real boot; GitHub Actions publication pending | Implemented locally |
| Update | Corrupted image is activated | Current/previous marker selection and user-data sentinel | Prototype; no signatures or rollback counter |

## Abuse cases and expected responses

- **Kernel ELF with a huge `p_memsz`:** rejected by checked arithmetic and a
  bounded image range; no copy occurs.
- **Overlapping `PT_LOAD` segments:** rejected so program-header order cannot
  choose which bytes win.
- **Entry point in BSS or outside the image:** rejected.
- **Memory map with more than 256 regions after merging:** loader stops with a
  diagnostic; it never truncates the map silently.
- **Overlapping or unsorted memory regions:** loader and kernel validation
  fail; the kernel halts with a diagnostic.
- **Invalid memory kind in `BootInfo`:** kernel validation fails and halts.
- **Kernel entry pointer null:** kernel prints `AETHER_BOOT_FAILED` and halts.
- **Forged capability handle or wrong endpoint:** the capability lookup fails
  before any message is queued or consumed.
- **Full receiver capability table:** the message remains queued and can be
  received after the receiver makes room; no capability is silently dropped.

## Known gaps

- The loader is trusted and its own image is not verified by the kernel.
- Services and the desktop still execute as kernel modules. Their capability
  profiles and supervision rules are policy prototypes, not a completed
  service-isolation boundary.
- The native application is the only ring-3 component; there is no dynamic
  process creation, waiting, or per-process resource accounting.
- Per-task address spaces are tested by positive boot-time checks; there is no
  fuzzing or deliberately broken configuration in CI.
- NX and WP are enabled and visible in the boot log, but there is no
  fault-injection test that attempts a write to kernel text or an instruction
  fetch from data.
- IPC is polled, not blocking; there is no wakeup queue, timeout, cancellation,
  or endpoint-destruction path. General capability revocation and complete
  object lifetime are not implemented.
- The filesystem is a volatile RAM disk with a bring-up format; it is not
  crash-consistent and does not survive reboot.
- Portal grants are seeded at boot. There is no interactive consent, persistent
  grant store, or revocation UI.
- The kernel identity-maps physical memory and caps the map bitmap at 16 GiB;
  there is no higher-half mapping, kernel heap, or guard page.
- The boot image is unsigned. Secure Boot, signed updates, artifact
  verification, and rollback protection are later epics.
- Serial output is not rate-limited or access-controlled; it is a development
  channel, not a production logging interface.
- The test suite is not a security proof. A passing boot test only demonstrates
  that the expected path reached the marker.

## Review triggers

Re-review this document when adding: user mode, page tables, capability
objects, IPC, a filesystem, device drivers, networking, package verification,
updates, or any code that parses data from an untrusted source.
