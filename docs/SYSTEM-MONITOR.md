# System Monitor

**System Monitor** is a trusted native app with exactly `CAP_WINDOW` and
`CAP_SYSINFO`. It cannot read files, spawn programs, or change settings.

It samples once a second (and on input) with:

- `SYS_SYSINFO` for total/free memory, uptime, CPU model/features and process
  count;
- `SYS_PROC_LIST` for pid, parent, state, name, trust, DLP taint, capabilities
  and memory;
- `SYS_FS_INFO` for storage kind and free space;
- `SYS_NET_INFO` for link state and the IPv4 address;
- `SYS_OS_INFO` for the OS version and build.

The window is resizable (`EV_RESIZE`), and `Esc` closes it. Start it from the
dock or with `monitor` in the Terminal.

The process table is kernel data: an app can only see it with `CAP_SYSINFO`.
The monitor's own window is not privileged; it gets the same untrusted badge
rules as any other app if its manifest changes.
