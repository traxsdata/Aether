# Compatibility Classification and Database

**Document ID:** AETHER-COMPAT-001
**Status:** Implemented as a database and classification harness; no Windows
runtime exists
**Related:** [Windows feasibility plan](windows-compatibility.md),
[host-interface report](windows-host-interface-report.md), ADR-007

## Purpose

Every tested application receives exactly one classification, backed by named
tests and a test date. The database makes claims reviewable: a record cannot say
"verified" without naming the tests that passed, and an unsupported application
must state why.

## Classifications

| Classification | Meaning |
|---|---|
| `verified` | Reproducibly passes the Aether test suite. |
| `compatible` | Works with documented limitations. |
| `limited` | Some functionality fails or requires a workaround. |
| `unsupported` | Known incompatibility or an unmet security requirement. |

`aether-compat::classify` derives a classification from a suite run:

- an empty suite is `unsupported` (nothing was verified);
- a failure in a test whose name starts with `security.` is `unsupported`;
- any other failure is `limited`;
- a pass with a documented limitation is `compatible`;
- otherwise the result is `verified`.

Security-test failures cannot be downgraded to a limitation by editing the
database; they come from the suite result.

## Database format

`tests/compatibility/database.txt` is pipe-separated text:

```text
id | name | version | runtime | platform | tested | classification | tests | limitations
```

| Field | Rule |
|---|---|
| `id` | Stable reverse-domain identity (same rules as a package). |
| `name` | Display name. |
| `version` | Tested application version. |
| `runtime` | `native`, or a pinned compatibility runtime and version. `none` when no runtime exists. |
| `platform` | Virtual or physical hardware used for the test. |
| `tested` | Date of the last completed run (`YYYY-MM-DD`). |
| `classification` | One of the four names above. |
| `tests` | Comma-separated test identifiers, or `none`. |
| `limitations` | Short documented limitation, or `none`. |

The parser rejects malformed lines, duplicate identities, invalid dates,
unknown classifications, a `verified`/`compatible` record without tests, and an
`unsupported` record without a reason.

A record must be updated only after a completed run. The database is a claim
ledger, not a security boundary.

## Current records

| Application | Runtime | Classification | Reason |
|---|---|---|---|
| Aether Demo | native | verified | Sandbox and portal tests pass on QEMU. |
| Aether Other | native | verified | Sibling-root denial test passes. |
| Aether Package Verifier | native | verified | Signature, digest, and publisher tests pass on host and in the boot self-test. |
| Notepad (Win32) | none | unsupported | No Windows runtime exists; the package verifier rejects `runtime=windows`. |
| GridCraft 0.8.0 (AppImage) | none | unsupported | No Linux layer exists; the payload needs glibc 2.35, Wayland, and Vulkan. See [Linux compatibility](linux-compatibility.md). |

## Test suite

`make compat-test` runs the database consistency tests. The harness API is
`aether_compat::classify` with named `TestResult` values; a future runtime
manager feeds it results and publishes nothing better than the suite supports.

Host-side package and update checks complement the database:

```sh
make unittest     # includes the package and update policy crates
make update-test  # package + update policy focused run
make test         # boot self-test for verification and rejection paths
```

## Process for adding a record

1. Run the application's test suite on a documented platform.
2. Add or update one record with the exact version, runtime, platform, date,
   tests, and limitations.
3. Keep the classification no better than the suite result.
4. Review the change like code. A claim of support is a public statement.

## Limits

- The database is not served over the network; it ships with the source tree.
- No Windows application has been executed; the only non-native record is
  explicitly `unsupported`.
- Accessibility and usability are not yet part of the classification.
