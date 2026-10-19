#!/usr/bin/env python3
"""make-signing-key.py - create an offline RSA-2048 release key.

The private key is written outside the source tree (or wherever --key points)
and must be kept secret. The public modulus is written to
common/release_key.h, which is the trust anchor built into the loader and
kernel. Replacing the key means re-signing every system image an installed
loader should accept.

  tools/make-signing-key.py --key /secure/aether-release.pem
  AETHER_SIGNING_KEY=/secure/aether-release.pem make images
"""
import argparse
import os
import subprocess


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--key", required=True)
    ap.add_argument("--header", default="common/release_key.h")
    a = ap.parse_args()
    if os.path.exists(a.key):
        raise SystemExit(f"make-signing-key: {a.key} already exists; refusing to overwrite a key")
    subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", a.key],
                   check=True)
    os.chmod(a.key, 0o600)
    modulus = subprocess.check_output(["openssl", "rsa", "-in", a.key, "-noout", "-modulus"],
                                      stderr=subprocess.DEVNULL).decode().strip().split("=", 1)[1]
    raw = bytes.fromhex(modulus)
    if len(raw) != 256:
        raise SystemExit("make-signing-key: unexpected key size")
    lines = []
    for i in range(0, 256, 16):
        lines.append("    " + ", ".join("0x%02x" % b for b in raw[i:i + 16]) + ",")
    header = '''/* release_key.h - the RSA-2048 release trust anchor.
 *
 * Generated from the offline release signing key. The private key is never
 * committed. Replacing this key requires re-signing every system image that
 * an installed loader will accept.
 */
#pragma once
#include <stdint.h>

#define AETHER_RELEASE_MODULUS_BYTES 256
#define AETHER_RELEASE_EXPONENT 65537
static const uint8_t aether_release_modulus[AETHER_RELEASE_MODULUS_BYTES] = {
%s
};
''' % "\n".join(lines)
    open(a.header, "w").write(header)
    print(f"make-signing-key: wrote {a.header}; keep {a.key} secret")


if __name__ == "__main__":
    main()