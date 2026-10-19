# Threat model

## Assets

- Integrity of the kernel and boot chain.
- Confidentiality and integrity of user files.
- Private application data and credentials.
- Capability tables and security policy.
- System availability and recoverability.

## Current scope

Aether is a bootable research prototype. The implemented boundaries are
firmware → loader, loader → kernel, and kernel-task → kernel-task address-space
isolation. There is no user mode, capability enforcement, IPC, or filesystem,
so those threats are recorded as planned work rather than mitigated today.

### In-scope adversaries

- A malformed or corrupted kernel image embedded in the loader.
- A firmware memory map with invalid, overlapping, or excessive regions.
- A loader/kernel version or ABI mismatch.
- A crafted `BootInfo` block or memory-region array that reaches the kernel
  through a bug or a future compromised loader.
- Accidental disclosure of secrets through serial diagnostics.
- A build or CI change that silently removes validation or the boot test.

### Out-of-scope for the prototype

- A malicious firmware or physical attacker with unrestricted hardware access.
- DMA attacks; there is no IOMMU configuration yet.
- Side channels, speculative-execution attacks, and microarchitectural
  isolation.
- Supply-chain compromise of the Rust toolchain or host OS.
- Multi-user isolation, because there are no users or applications yet.
- Windows compatibility runtime attacks, because no runtime exists.

## Boundaries and controls

| Boundary | Threat | Current control | Status |
|---|---|---|---|
| Firmware → loader | Oversized/overlapping/corrupt kernel ELF | Bounded parser, bounds and overlap checks, entry check, fixed segment limit | Implemented, host-tested |
| Firmware → loader | Memory map larger than loader storage | Fixed 64 KiB map buffer and 256-region array; fail closed | Implemented |
| Loader → kernel | Stale/wrong hand-off | Magic, version, structure size, count, kernel range, entry, region validation | Implemented |
| Loader → kernel | Invalid enum discriminant | Memory kinds stored as `u32` and decoded fallibly | Implemented |
| Boot path | Firmware page tables or NX deny execution | Boot test under OVMF; failure is visible on serial | Verified on QEMU/OVMF only |
| Boot path | Allocator reuses firmware page tables or kernel memory | Reserve kernel/hand-off; release boot services only after CR3 switch; double-free checks | Implemented, host-tested |
| Boot path | A fault during memory setup resets silently | IDT installed before paging; structured vector/RIP/CR2 report; fault self-test | Implemented, QEMU-tested |
| Kernel memory | Kernel text writable or data executable | 4 KiB W^X page permissions for the kernel image | Implemented; not yet adversarially tested |
| Kernel task → task | One task reads or writes another task's memory | Per-task PML4 and private frame at the same virtual address; CR3 switch on context switch; boot-time isolation test | Implemented, QEMU-tested |
| Kernel task → object | Forged handle, stale slot, or insufficient rights | Generation-checked capability handles; per-task tables; rights checks | Implemented, host- and QEMU-tested |
| Kernel task → IPC | Oversized message, queue exhaustion, capability forgery | 32-byte bound, four-message queues, critical sections, `TRANSFER` check | Implemented, host- and QEMU-tested |
| Kernel task → kernel | Task corrupts kernel state | Kernel and tasks share the kernel mapping; all tasks run at CPL0 | **Not isolated yet** (planned user mode) |
| Diagnostics | Secret leakage | Boot code prints only version, addresses, and memory sizes | Policy; no secrets exist yet |
| Build | Malformed image reaches CI as “passing” | `make test` requires a kernel marker after a real boot | Implemented |
| Kernel → user | User reads another task's memory | Not implemented | Planned (Epic C) |
| App → files | App reads ungranted files | Not implemented | Planned (Epic F) |
| Service → service | Impersonation or capability forgery | Not implemented | Planned (Epic D) |
| Update | Corrupted image is activated | Not implemented | Planned (Epic I) |

## Abuse cases and expected responses

- **Kernel ELF with a huge `p_memsz`:** rejected by checked arithmetic and a
  bounded image range; no copy occurs.
- **Overlapping `PT_LOAD` segments:** rejected so program-header order cannot
  choose which bytes win.
- **Entry point in BSS or outside the image:** rejected.
- **Memory map with more than 256 regions after merging:** loader stops with a
  diagnostic; it never truncates the map silently.
- **Invalid memory kind in `BootInfo`:** kernel validation fails and halts.
- **Kernel entry pointer null:** kernel prints `AETHER_BOOT_FAILED` and halts.

## Known gaps

- The loader is trusted and its own image is not verified by the kernel.
- Tasks are kernel tasks at CPL0 and share the kernel mapping; there is no
  user/kernel isolation.
- Capability and IPC enforcement is a kernel-internal API, not yet a user-mode
  syscall boundary.
- Per-task address spaces are tested by a positive boot-time check, not by an
  adversarial suite or a deliberately broken configuration.
- IPC is polled, not blocking; there is no wakeup queue, timeout, revocation, or
  object-lifetime test for endpoints.
- The kernel identity-maps physical memory; there is no higher-half mapping.
- The boot image is unsigned. Secure Boot, signed updates, and rollback
  protection are later epics.
- Serial output is not rate-limited or access-controlled; it is a development
  channel, not a production logging interface.
- The test suite is not a security proof. A passing boot test only demonstrates
  that the expected path reached the marker.

## Review triggers

Re-review this document when adding: user mode, page tables, capability
objects, IPC, a filesystem, device drivers, networking, package verification,
updates, or any code that parses data from an untrusted source.
