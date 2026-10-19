# æther security model

Security is the starting point of the design, not a feature added later.
The rule is **least privilege everywhere**: no code, kernel or app, can do
more than its job needs, and the hardware enforces it wherever possible.

## Isolation

- **Every app is a separate user-mode (ring 3) process** with its own page
  tables. It can see only its own code, data, stack and window buffer.
- **Kernel memory is supervisor-only.** An app that reads or writes it is
  stopped by a page fault and terminated, with a notification on screen.
- **No hardware access from apps.** The TSS has no I/O permission bitmap and
  IOPL is 0, so `in`/`out`, `cli`, `hlt` etc. raise a protection fault.
- **Input goes only to the focused window's process**, so a background app
  can't log keystrokes. Window chrome, the dock and notifications are drawn
  by the kernel, so apps can't fake them.
- **A background app cannot take focus.** Only an app's first window, after
  the user launched it, is focused automatically; later windows go to the
  bottom of the stack until the user clicks them.
- **Passphrase change.** Settings > Security checks the old passphrase and
  re-wraps the volume key under the new one; the volume key itself never
  changes and neither passphrase is stored. A failed write leaves the old wrap
  intact.
- **Session lock.** Super+L or the æ menu locks the desktop. The kernel draws
  an opaque lock screen, stops delivering input, and verifies the passphrase
  against the AFS key-wrap record without changing the mounted file system.
  Settings > Security sets an idle auto-lock (10 minutes by default).

## Capabilities

Each process carries a capability mask, fixed at spawn time:

| Capability | Allows | Terminal | Calculator | Notes | Probe | Files | Settings | Store |
|------------|--------|:-------:|:-------:|:-------:|:-------:|:-------:|:-------:|:-------:|:-------:|
| `CAP_WINDOW` | create windows, receive input | ✓ | ✓ | ✓ | | ✓ | ✓ | ✓ |
| `CAP_LOG` | write to the kernel log | ✓ | | | | | | | |
| `CAP_SYSINFO` | read system info, process list, kernel log | ✓ | | | | | ✓ | |
| `CAP_SPAWN` | start programs | ✓ | | | | | | ✓ |
| `CAP_KILL` | stop other processes | ✓ | | | | | | |
| `CAP_FILES` | open files by path | ✓ | | | | ✓ | | |
| `CAP_PICK` | ask you to choose a file (system picker) | ✓ | | ✓ | | | | ✓ |
| `CAP_STORAGE` | a private folder, `/apps/<name>` | ✓ | | ✓ | ✓ | | | ✓ |
| `CAP_NET` | outgoing TCP, checked by the firewall | ✓ | | | | | ✓ | ✓ |
| `CAP_FIREWALL` | read/change firewall rules | ✓ | | | | | ✓ | |
| `CAP_APPS` | install, update and remove app packages | ✓ | | | | | | ✓ |
| `CAP_CONFIG` | network and system settings | ✓ | | | | | ✓ | |
| `CAP_UPDATE` | install the OS, write A/B slots, reboot | ✓ | | | | | | ✓ |

A child gets the intersection of what its program's manifest allows
(`kernel/apps.c`), what the parent asked for, and what the parent itself
holds, so capabilities can only shrink, never grow.

## Trust levels and the sandbox

Every program also has a trust level (`kernel/apps.c`): **system**
(Terminal), **trusted** (Calculator, Files, Settings, App Store) or
**untrusted** (Notes, Probe, and every package installed by App Store). A
child never gets a higher trust level than its parent.

An untrusted app additionally:

- may only make the system calls on an allowlist (windows, events, time,
  files through its handles, memory, and outgoing connections). Anything
  else is refused before the call even looks at its arguments, and the
  attempt is logged.
- has a **memory quota** (`SYS_MEM_MAP` memory is counted against it).
- **can't create executable memory**: no `PROT_EXEC` at all, so it can only
  run the code it was shipped with.
- gets an orange **untrusted** badge on its windows, drawn by the kernel.

### Installed apps

App Store downloads are attacker-controlled input. The kernel checks the
package header, name, SHA-256 and the whole ELF image before writing it to
`/system/apps`, a folder no app can resolve. The optional package icon is
bounds-checked, size-limited and hashed like the ELF; it is only ever drawn,
never executed. An installed app is always untrusted, whatever its header
claims, and may only hold `CAP_WINDOW`, `CAP_PICK`, `CAP_STORAGE` and
`CAP_NET`. System names are reserved, so a
package can't shadow Terminal, Settings or any of the OS's own apps. A
running app is never restarted by an update: the new package waits for its
next start and the window title says `update pending`. See
[APPS.md](APPS.md).

Files, encryption and data labels are described in [DLP.md](DLP.md).

## Memory protections

- **W^X**: no page is ever writable and executable. Kernel `.text` is
  read+execute, `.rodata` read-only, data/stack/heap no-execute. The kernel
  refuses to load an app whose ELF asks for a writable+executable segment.
- **NX** is required: æther won't boot on a CPU without it.
- **SMEP** (kernel can't execute app memory) and **SMAP** (kernel can't read
  or write app memory except inside `copy_in`/`copy_out`) are enabled when
  the CPU supports them; **UMIP** hides kernel table addresses from apps.
- **No identity map and no page 0**: null-pointer dereferences fault.
- **Guard pages** under every kernel stack; double faults and NMIs run on
  their own known-good stacks (IST), so even a kernel stack overflow ends in
  a readable crash screen instead of a silent reset.
- **Stack canaries** (`-fstack-protector-strong`) in the kernel and apps,
  seeded from RDRAND when available.

### System dialogs

A dialog records **the exact thread** that opened it, and only that thread is
woken with the answer. Network consent, a label lowering, a file choice and a
destructive confirmation can no longer be consumed by a sibling thread of the
same app. Each dialog type has its own refusal budget, and `Ctrl+Alt+Esc`
closes the requesting app if a picker loop would otherwise trap the user.

## The system call boundary

`kernel/syscall.c` is the only door into the kernel. Every call checks the
caller's capability first, then validates every pointer against the
caller's own page tables (present, user-accessible, writable when the
kernel will write) before touching it. Copies are bounded, strings are
length-limited, and log messages are stripped of control characters.

## Linux programs (0.4)

Apps built as static Linux programs (the Craft apps) make Linux system calls,
which `kernel/linux.c` answers with æther's own rules rather than around
them; see [RUNNER.md](RUNNER.md). Installed app packages always run
untrusted, whatever their header asks for; their image must match its
SHA-256 before it runs. There is no fork, exec, or signalling another app
(and, since 0.5, network only through the firewall, below); `mmap`/`mprotect` can never make memory writable and
executable, and untrusted apps can't make new executable memory at all.
Memory they reserve counts against their limit when touched, and the kernel
checks that limit again when it touches their memory on their behalf.
`test runner` checks these rules, and `runner/linuxtest` tries ten attacks
from inside a real Rust program.

## The network (0.5)

Nothing listens: æther answers no unsolicited packet, not even a ping.
Every outgoing connection needs `CAP_NET` and passes, before any packet
is sent, DLP (no connections after reading Confidential data), the
firewall (rules by app and host name; with no rule the kernel asks you in
a dialog apps can't fake), and an address check (no loopback, link-local
or multicast, and no LAN address reached through a name, which blocks DNS
rebinding). Rules are stored where no app can reach them. DNS uses random
ids and ports and parses replies strictly; TCP uses random sequence numbers
and RFC 5961 checks. Details in [NETWORK.md](NETWORK.md).

## Seeing it work

Run `sectest` in the Terminal. It starts `probe`, an untrusted program
with nothing but a private folder, twelve times, and each time it tries a
different attack:

1. read kernel memory
2. overwrite its own code
3. execute code placed on its stack
4. run a privileged instruction
5. access hardware I/O ports
6. use a capability it doesn't have
7. pass a kernel pointer to a system call
8. start a more privileged program
9. open your files by name
10. climb out of its own folder with `..`
11. make writable memory executable
12. take more memory than its quota

All twelve must be blocked. The test reports how each one was stopped.

## Not done yet (see ROADMAP.md)

- Secure Boot signing of the loader and kernel, and a measured boot chain.
- Signed app packages (authenticity as well as integrity).
- KASLR (randomized kernel and app addresses) and ASLR for apps.
- Kernel page-table isolation (Meltdown-class mitigations) and other
  speculative-execution hardening.
- A persistent, permission-checked file system; per-user accounts.
- More ring-0 drivers in Rust (0.8 starts with NVMe); see [DRIVERS.md](DRIVERS.md).
- **Installer boot images are pinned in the signed system image.** The
  UEFI loader is not updated by an OS update, so the 1.0 system image carries
  the SHA-256 of the matching boot image in its signed header; a fresh
  install refuses another one. App packages are still integrity-checked, not
  signed (see below).
- **Boot-control is not a hardware root of trust.** Its records are CRC32
  checked, not signed. Someone with raw disk write access can lower the
  rollback floor or point at an older, genuinely signed image. They can also
  overwrite the slots themselves, so closing this needs TPM/RPMB or signed
  boot state.
