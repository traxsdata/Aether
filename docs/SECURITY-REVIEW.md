# Adversarial review of æther 0.5 (October 2026)

A review of main after 0.5 (networking, firewall, browser, Files) and TLS 1.2,
looking for ways an untrusted app (a Craft app, the browser, any installed
package), a hostile network or a hostile disk could break æther's promises.
Everything below was found by reading the code; each fix comes with a
regression test where one could be written.

Severity is for æther's own threat model: untrusted apps must not affect the
kernel, other apps or data they weren't given, and DLP must hold.

## Fixed

| # | Severity | Weakness | Fix |
|---|----------|----------|-----|
| 1 | High | A Linux-ABI app could start a thread whose TLS pointer (`clone` with `CLONE_SETTLS`) wasn't a user address. The kernel loads it into FS_BASE at the next switch to that thread, which faults inside the kernel: any untrusted app could crash the whole machine. `arch_prctl` already checked this; `clone` didn't. | `kernel/linux.c` refuses it; `runner/linuxtest` checks it. |
| 2 | High | Network calls that wait (`net_recv`, `net_send`, `net_connect`) kept a pointer to the connection across the wait. If another thread of the app closed the handle meanwhile, the slot could be freed and handed to a different app's new connection, and the waiting call would then read that app's incoming data, or write into its stream, bypassing the firewall. | `kernel/net.c` looks the handle up again after every wait and gives up if it's gone. |
| 3 | Medium | DLP: an untrusted app holding Confidential data couldn't write it anywhere unlabelled, but file *names* and *sizes* in its own folder carried no label. A later, untainted run of the same app could list the folder and send the names over the network. | Folder entries are labelled data for untrusted apps (`kernel/vfs.c`, `docs/DLP.md`). `ftruncate` and `O_TRUNC` now raise a file's label like a write. |
| 4 | Medium | Lowering a label: after you approved "Personal → Public" for a file, the kernel re-read the file but didn't check it was still what you were shown. Another thread could write Secret data into it while the dialog was up (raising it to Secret), and your approval would then make it Public. | `sys_set_label` refuses if the label or the file changed while the dialog was open. |
| 5 | Medium | System dialogs acted on the first key or click. An app could open the file picker just as you pressed Enter in its window: Open would hand it the first file in Home, and Save with a name the app suggested would empty an existing file of yours. | Dialogs ignore input for 0.7 s after they appear, and saving over an existing file needs a second Save (`kernel/wm.c`). |
| 6 | Medium | Memory limits didn't count page tables. Reserving one page in each of many 1 GiB stretches costs almost nothing against the quota but a page table or two each, so an untrusted app could drain the machine's memory. | Page tables made for an app's reservations count against its quota (`kernel/umem.c`, `kernel/vmm.c`); new unit test. |
| 7 | Low | ELF loader: the program-header bounds check could overflow with a huge `e_phoff`, making the kernel read before the image while loading a crafted package. | Overflow-safe check in `kernel/elf.c`; unit test. |

## Not fixed (noted for the roadmap)

- **Package authenticity.** A package's SHA-256 sits in its own header, so it
  catches damage, not tampering. Firewall rules are keyed by app name, so a
  replaced package with the same name inherits "Always allow". Signing
  packages (and Secure Boot) would close both.
- **Disk rollback.** Sealed blocks are authenticated and bound to their file
  and position, but an attacker with the disk can put back an older copy of a
  block, and the free-space bitmap isn't sealed. Needs a version or Merkle
  root in the superblock.
- **Thread and connection slots are system-wide** (64 kernel stacks, 64 TCP
  connections; 32 threads and 16 connections per app), so a few greedy apps
  can stop others starting. A resource-exhaustion annoyance, not a breach.
- **LAN attackers** can spoof ARP and DHCP like on any OS. TLS protects the
  browser; the Terminal's plain-http `get` is not protected.

## What held up

The system call boundary (pointer checks against the caller's own page
tables, SMAP windows), W^X everywhere including the Linux layer, the
untrusted-app syscall allowlist, the TCP/IP parsers (lengths checked at every
layer, RFC 5961, strict DNS parsing, no listening sockets), the firewall's
address policy, the encrypted file system's sealing, and the browser's
certificate path building (basicConstraints, keyUsage, EKU, name
constraints, RFC 6125 host matching) all looked sound.
