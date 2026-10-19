# Running Linux applications on æther

Short answer: æther already runs **static Linux-ABI programs**, and the Craft
apps prove it. It does **not** run ordinary dynamically linked Linux
applications, and it is not close to running Steam. The gap is not one
syscall; it is a compatibility runtime, a filesystem namespace and a bundle
format.

This document separates what works today, what a realistic next step looks
like, and what is out of reach.

## What works today

`kernel/linux.c` answers the Linux system-call ABI for a process with
`abi = ABI_LINUX`. The rules are æther's, not Linux's:

- **Static, position-independent ELF only.** `elf_check()` accepts `ET_EXEC`
  and `ET_DYN`, but rejects `PT_INTERP`. A glibc binary that expects
  `/lib64/ld-linux-x86-64.so.2` will not start.
- **No `fork`, `execve`, `vfork` or `clone3`.** Threads are supported
  (`CLONE_VM | CLONE_SIGHAND | CLONE_THREAD`); there is no second address
  space and no program replacement.
- **No writable+executable memory**, and untrusted apps cannot create
  executable memory at all.
- **A private folder** (`/apps/<name>`) and the system file picker. A Linux
  app almost never gets `CAP_FILES`; it cannot name the host's files.
- **Outgoing TCP only**, through `CAP_NET`, DLP and the firewall.
- **Threads, futexes, mmap/brk, epoll, pipes, Unix sockets, TLS in user space**
  and the common file calls are implemented to the extent the Craft apps need
  them.

That is enough for a self-contained Rust or C program compiled with musl and
linked statically. It is not enough for Node.js, Electron, Steam, a browser
engine shipped as a native binary, or a program that shells out to `git` or
`rg`.

## Three ways to run "Linux apps", and what they cost

### 1. Native ports (best security, best performance)

Rebuild the app for the æther ABI or the static Linux layer. The app gets one
address space, one set of capabilities, a private folder and only the
specific calls it needs. This is how the Craft apps and the in-tree Rust
apps work.

This is the right path for anything the project controls. It is not a way to
run the existing Linux ecosystem unchanged.

### 2. A self-contained Linux runtime ("app containers" without Linux)

This is the realistic middle path: an app bundle carries its own read-only
runtime and filesystem, and the kernel gives it a namespace instead of the
host's filesystem.

```
<app>.app
├── manifest          name, entry point, caps, network hosts, ABI
├── rootfs.img        read-only: /bin, /lib, /usr, app files
├── signature         publisher signature over the header + hashes
└── icon
```

At start:

- `/` inside the process maps to `rootfs.img` through a per-process root
  inode, never to æther's real root.
- Missing writable files go to an encrypted per-app overlay
  (`/apps/<name>/overlay`), so the image stays immutable and updates are
  atomic.
- `PT_INTERP` is resolved **inside the image** (`/lib64/ld-linux...` or a
  musl loader). Shared libraries are loaded from the image, with the loader's
  search path confined to it.
- `fork`/`exec` are allowed only for programs inside the same bundle. A child
  inherits the parent's sandbox, capabilities and DLP label; it cannot exec a
  path outside the image, gain a capability, or reach the host filesystem.
- `exec` of a file written in the overlay is refused unless the bundle
  declares it as a verified tool (a package manager inside the sandbox must
  not become an escape hatch).
- The kernel keeps the same protections: W^X, no hardware access, per-process
  memory quota, no ptrace, no kernel addresses, network only through the
  per-app firewall.
- The bundle is signed like an OS image, so the runtime and libraries are
  authentic, not just integrity-checked.

The 1.1a foundation (per-process root, read-only root and file copy-up overlay)
is in the tree; the missing kernel work is bounded and testable:

1. per-process root/namespace in `vfs.c` (an app cannot resolve `..` out of
   its root; `/system` and `/home` are not reachable),
2. read-only image + writable overlay in AFS,
3. `PT_INTERP`/`ld.so` loading with an auxv and library search path rooted in
   the image,
4. sandboxed `fork`/`exec` and a process tree that inherits caps and taint,
5. `/proc/self`, PTY, more signals, `eventfd`/`timerfd` and Unix sockets as
   needed by the runtime,
6. a signed bundle format and publisher UI.

This is a compatibility *runtime*, not a Linux kernel. It will run a useful
subset of CLI tools and static or bundle-provided runtimes. It will not run
programs that need kernel modules, device nodes, X11/Wayland, GPU APIs or
32-bit binaries.

### 3. A full Linux VM for Steam-class software

Steam, Proton and SteamOS games need a real Linux userspace: dynamic glibc,
X11/Wayland, Vulkan/OpenGL, audio, D-Bus, 32-bit support, GPU drivers and a
large kernel surface. Reproducing that inside æther's syscall layer is not a
sensible 1.x goal.

The realistic options are:

- **Native game ports** to æther (the same model as the Craft apps), or
- **a remote-play client** (Steam Link protocol) that turns Steam on another
  machine into an æther app, or
- **a Linux VM** with hardware virtualization. That is a separate world with
  its own kernel; it weakens the "one small trusted kernel" story and needs
  VM device drivers.

SteamOS itself is an operating system, not an application. "SteamOS works"
really means "Steam and its games run", which is tier 3.

## Claude Code specifically

Claude Code is a Node.js/TypeScript CLI. It needs:

- a Node (or Bun) runtime in the bundle,
- a dynamic loader and shared libraries **or** a fully static runtime,
- `fork`/`exec` for `git`, `rg`, the shell and tool subprocesses,
- a raw-mode PTY with ANSI handling,
- a working set of file APIs, `epoll`, signals and `/proc/self`,
- HTTPS to the API, mediated by the per-app firewall,
- a workspace the user grants, with no ambient access to the rest of the disk.

Tier 2 can support this if the bundle ships a runtime and its tools. It should
run in a dedicated window or in a future terminal that implements raw mode,
not as a shell built into the kernel. The security story is good: the app is
untrusted, has no host files by default, gets one workspace through the
picker, and can only reach the hosts the user approves.

The practical first milestone is not "run every Linux binary". It is:

> run one signed self-contained bundle, with its own read-only rootfs and
> writable overlay, that can spawn its own child tools and reach exactly one
> approved network destination.

That is also the right foundation for a Linux-compatible CLI environment
without trusting Linux binaries with the host.

The milestone breakdown and exit criteria are in
[PLAN-1.x.md](PLAN-1.x.md).

## What to build first

If this becomes a 1.x workstream, start with the smallest end-to-end proof:

1. A read-only AFS image containing `/bin/hello` and a static musl loader.
2. Per-process root resolution and an overlay, with tests that try `..`,
   absolute host paths and `/system`.
3. A bundle manifest and signature, and a test that a changed rootfs or
   library is refused.
4. `exec` inside the bundle with capability and taint inheritance, and a test
   that `exec /system/...` is refused.
5. A tiny PTY test program.
6. Then the first real app: a Node/Bun CLI bundle that makes one HTTPS
   request and runs one child tool.

Until then, the honest description is: **æther runs static Linux-ABI apps,
not native Linux applications.** The compatibility runtime is a deliberate
project, not a switch.
