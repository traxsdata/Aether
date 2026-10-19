# Settings

Settings is a trusted, clean-room Rust app with exactly the capabilities its pages need:

| Capability | Used for |
|---|---|
| `CAP_WINDOW` | its window |
| `CAP_SYSINFO` | CPU, memory, storage and the DLP audit log |
| `CAP_NET` | reading link/address counters |
| `CAP_FIREWALL` | listing, adding and removing firewall rules |
| `CAP_CONFIG` | network configuration and `/system/settings` |

It never opens the user's files.

## Network

The Network page reads `SYS_NET_INFO` and `SYS_NET_CONFIG`.

- **DHCP** is the default. The page shows the address, netmask, router and DNS
  server the DHCP lease provided.
- **Static** lets you set the four addresses. `SYS_NET_CONFIG` applies them
  immediately, saves them to `/system/settings`, and stops DHCP from replacing
  them. Switching back to DHCP restarts the DHCP client.
- Existing TCP connections are not migrated when the address changes; a
  connection made after the change uses the new address.

`SYS_NET_CONFIG` requires `CAP_CONFIG`. The kernel validates the mode and
requires a non-zero address and netmask. The settings are plain `key=value`
lines in `/system/settings`, a file no app can open.

## Wi-Fi

There is no Wi-Fi driver yet. The Wi-Fi page reports "No Wi-Fi adapter found"
instead of pretending to scan. The wired Intel PRO/1000 driver is the supported
network card today; Wi-Fi for real laptops is a roadmap item.

## Firewall

The Firewall page lists the rules the kernel stores in `/system/firewall`:

- the first rule matching app, host and port decides;
- with no rule, the kernel asks when an app connects;
- outgoing only: nothing listens, so inbound traffic is always refused;
- `CAP_FIREWALL` is held by Terminal and Settings, not by ordinary apps.

## Security

The Security page sets the idle auto-lock (Never, 5, 10 or 30 minutes; the
default is 10) with `security.autolock` in `/system/settings`, and has a
**Lock Now** button (`SYS_LOCK`).

It also changes the disk passphrase. The kernel checks the current passphrase
against the mounted volume's key-wrap record, derives a new key with the
volume's existing salt and PBKDF2 parameters, re-wraps the random volume key
and writes the superblock. Neither passphrase is stored; a failed write leaves
the old wrap intact. The page refuses a new passphrase shorter than eight
characters or a mismatch.

## Privacy

The Privacy page shows the storage kind (encrypted or temporary), free space,
the label this app itself holds, and the DLP audit log. The audit log is the
kernel's, not a copy: raising and lowering labels, reads that taint an app, and
blocked egress all appear there.

## About

The About page reads `SYS_OS_INFO` and `SYS_SYSINFO`: version, build time,
uptime, CPU, memory, process count and which hardware protections (NX, SMEP,
SMAP, UMIP, RDRAND) are active.

## System calls added for Settings

| Call | Capability | Purpose |
|---|---|---|
| `SYS_NET_CONFIG` | `CAP_CONFIG` | get or set DHCP/static configuration |
| `SYS_SETTINGS_GET` | `CAP_CONFIG` | read a key from `/system/settings` |
| `SYS_SETTINGS_SET` | `CAP_CONFIG` | write a key |
| `SYS_OS_INFO` | none | version, build and uptime |
| `SYS_WIFI_INFO` | none | adapter state (no Wi-Fi driver yet) |
| `SYS_LOCK` | `CAP_CONFIG` | lock the desktop now |
| `SYS_CHANGE_PASSPHRASE` | `CAP_CONFIG` | re-wrap the volume key after checking the old passphrase |
| `SYS_SHUTDOWN` | `CAP_CONFIG` | ACPI S5 power off |

`SYS_FIREWALL` now checks `CAP_FIREWALL` rather than requiring the whole
`TRUST_SYSTEM` level, so Settings can manage rules without becoming a system
process.
