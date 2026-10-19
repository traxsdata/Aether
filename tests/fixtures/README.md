# Test fixtures

These files support the package and update verification tests. They are
**fixtures, not releases**.

## Keys

`keys/test.seed` and `keys/untrusted.seed` are **test keys**. They are public and
provide no security. Never sign anything outside this repository's tests with
them, and never reuse a fixture key for a release.

`keys/test.pub` is derived from the test seed. The kernel trust store embeds the
public key so the boot self-test can accept the valid fixtures and reject the
rest. Regenerating the keys also regenerates the public key file, so the kernel
and fixtures stay consistent.

## Layout

```text
keys/                       Ed25519 test seeds and public keys
packages/payload/           source files for the demo package
packages/manifests/         source manifests (the publisher key is injected)
packages/demo.aether        valid package, signed by test.seed
packages/untrusted.aether   signed by untrusted.seed (not in the trust store)
updates/images/             image bytes referenced by the update manifests
updates/manifests/          current, previous, downgrade, forged manifests
```

The update fixtures use installed state `0.1.0`/counter `1`:

| Manifest | Version | Counter | Key | Expected result |
|---|---:|---:|---|---|
| `current` | 0.2.0 | 2 | test | accepted |
| `previous` | 0.1.0 | 1 | test | rejected: rollback |
| `downgrade` | 0.0.9 | 3 | test | rejected: downgrade |
| `forged` | 0.3.0 | 4 | untrusted | rejected: unknown publisher |

## Regenerating

```sh
. "$HOME/.cargo/env"
sh tests/fixtures/build.sh
```

The script keeps existing seeds, rebuilds the packages and manifests, and
verifies the valid fixtures. Review any change to the fixture bytes: the kernel
embeds them at compile time.
