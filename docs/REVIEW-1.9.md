# Review of 1.9: System Monitor

Based on `v1.8`. This delivers the first item from the roadmap's "More apps"
line: a system monitor, without giving an app privileged kernel access.

## Changes

- `user/monitor.c`, a native C app built into the kernel image. It draws with
  the same `lib/ui` surface as Calculator and Notes.
- Capabilities are exactly `CAP_WINDOW | CAP_SYSINFO`, trust is trusted, quota
  32 MiB. No file, spawn, network, clipboard or config capability.
- The window samples `SYS_SYSINFO`, `SYS_PROC_LIST`, `SYS_FS_INFO`,
  `SYS_NET_INFO` and `SYS_OS_INFO` once a second, on input and after a resize.
  It lists pid, name, trust, DLP taint, capability mask and memory.
- Added to the program table, embedded app blobs, dock, Makefile and Terminal
  (`monitor` command). `make test-monitor` boots QEMU on temporary storage,
  starts it, and checks the serial log.
- Kernel test: the manifest and spawned process have only the window and
  system-info capabilities.
- Version strings were stale at 0.9.0; the kernel and default image version
  are now 1.9.0.

## Evidence

```text
make            PASS
make unittest   61 kernel / 73 terminal, 0 failed
make test-monitor PASS
```

The screenshot shows 3 processes (kernel, terminal, monitor), memory and
storage, and the monitor's own `trusted` / `Public` row.

## Not done

- Per-process CPU time and true CPU utilisation need scheduler accounting.
- Kill buttons and an app-data view: the monitor is read-only on purpose.
- Shared-memory and message-passing IPC (the rest of the roadmap's "More apps"
  line) are not in this slice.
- Wi-Fi, audio, battery/lid, camera, Bluetooth, suspend and thermal policy
  remain hardware work from 1.5.
