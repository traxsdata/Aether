# Contributing to Aether

Aether is an independent kernel and desktop project. Contributions are welcome
when they preserve the project's security model and can be reviewed.

## Before you start

- Read the [technical specification](Technical%20Specification%20and%20Initial%20Development%20Backlog.txt),
  especially the security architecture and backlog execution rule.
- Check whether the change belongs to the current phase. The first sprint
  intentionally excludes schedulers, filesystems, graphics, and Windows
  runtimes; adding them early makes review harder, not progress faster.
- For a substantial design change, open an issue or draft ADR first.

## Development workflow

```sh
git fetch origin
git switch -c feat/<short-topic> origin/main
# edit
make ci
git add <files>
git commit -m 'feat: <short description>'
git push -u origin feat/<short-topic>
```

`make ci` runs formatting, clippy, documentation link checks, licence checks,
host unit tests, the QEMU boot test, the fault-path test, and a negative boot
test that proves the harness rejects a deliberately failing kernel. The gate is
intended to run in GitHub Actions; until the workflow is published, run it
locally before opening the pull request.

Open a pull request against `main`. Keep the branch rebased on current `main`;
do not force-push a branch that someone else is reviewing unless you have
amended your own unpublished commit and use a lease. Do not commit `target/`,
`build/`, serial logs, editor configuration, or credentials; they are ignored
by `.gitignore`.

### Commit messages

Use a short imperative subject with a conventional prefix:

- `feat:` new functionality
- `fix:` bug fix
- `docs:` documentation only
- `test:` tests only
- `build:` build, CI, or toolchain changes
- `refactor:` behaviour-preserving code change
- `security:` trust-boundary or hardening change

Explain *why* in the body when the change is not obvious. Do not include
credentials, tokens, personal data, or generated binaries.

### Pull request description

Every PR should state:

1. What changed and why.
2. The exact commit tested.
3. Commands run and their pass/fail counts (`make ci`, plus any manual QEMU
   checks). If you run the individual targets instead, list each one.
4. Anything that was **not** run, and why.
5. Known limitations, timing caveats, or follow-up work.

A claim that the system boots must include the serial transcript or the
`AETHER_BOOT_OK` marker from the tested commit.

## Code expectations

- Kernel and loader code is `no_std`; do not add dependencies without a clear
  justification and a review of their licences and attack surface.
- Every `unsafe` block needs a `SAFETY:` comment explaining the invariant.
- Validate all data that crosses a privilege or trust boundary. A loader or
  firmware input is untrusted until checked.
- Prefer fixed bounds and explicit error handling over panics in boot paths.
- Keep public interfaces documented; the ABI is versioned and stable within a
  major prototype version.
- Add host tests for parsing, arithmetic, and policy logic. Boot tests cover
  what cannot be tested on the host.
- Run `cargo fmt` before submitting. `make lint` must pass.

## Security changes

Changes to the boot hand-off, capability model, IPC, memory isolation, or
privilege checks are security-critical. Say so in the PR title or body and
expect a slower review. Do not disclose an unfixed vulnerability in a public
issue; use the process in [`SECURITY.md`](SECURITY.md).

## Licensing

By contributing, you agree that your contribution may be distributed under the
project licence (Apache-2.0 at the time of writing). The specification notes
that the final licence and any contributor agreement require review before a
public release; this section will be updated if that changes.
