#!/usr/bin/env python3
"""mkroots.py - turn a PEM bundle of root certificates into the browser's
trust anchors, runner/webnet/src/roots_data.rs.

Usage:
    tools/mkroots.py [--source TEXT] [--keep-expired] INPUT... > runner/webnet/src/roots_data.rs
    tools/mkroots.py -o runner/webnet/src/roots_data.rs /usr/share/ca-certificates/mozilla

Each INPUT is a PEM file (a bundle like ca-certificates.crt or Mozilla's
cacert.pem) or a directory of *.crt / *.pem files. For every certificate we
keep only what path validation needs: the subject name and the public key
(both as DER) and the expiry date. Duplicates are dropped, as are roots
that have already expired (unless --keep-expired).

Trust has to come from somewhere: these roots are the one piece of external
data the browser needs. Review the input before regenerating: anything in
it can vouch for any web site.
"""
import argparse
import base64
import calendar
import hashlib
import os
import sys
import time


def der_read(b, i):
    """Return (tag, content_start, content_end, element_end) at offset i."""
    tag = b[i]
    l = b[i + 1]
    j = i + 2
    if l & 0x80:
        n = l & 0x7F
        if n == 0 or n > 4:
            raise ValueError("bad DER length")
        l = int.from_bytes(b[j:j + n], "big")
        j += n
    if j + l > len(b):
        raise ValueError("truncated DER")
    return tag, j, j + l, j + l


def parse_time(tag, s):
    s = s.decode("ascii")
    if tag == 0x17:
        yy = int(s[:2])
        year = 2000 + yy if yy < 50 else 1900 + yy
        rest = s[2:]
    elif tag == 0x18:
        year = int(s[:4])
        rest = s[4:]
    else:
        raise ValueError("bad time")
    if not rest.endswith("Z") or len(rest) != 11:
        raise ValueError("bad time")
    t = (year, int(rest[0:2]), int(rest[2:4]), int(rest[4:6]), int(rest[6:8]), int(rest[8:10]))
    return calendar.timegm(t + (0, 0, 0))


def common_name(name):
    """The CN of a DER Name (else OU, else O), for a comment."""
    _, s, e, _ = der_read(name, 0)
    attrs = {}
    i = s
    while i < e:
        _, ss, se, i = der_read(name, i)  # SET
        j = ss
        while j < se:
            _, as_, ae, j = der_read(name, j)  # AttributeTypeAndValue
            _, os_, oe, k = der_read(name, as_)
            _, vs, ve, _ = der_read(name, k)
            attrs[name[os_:oe]] = name[vs:ve].decode("utf-8", "replace")
    for oid in (b"\x55\x04\x03", b"\x55\x04\x0b", b"\x55\x04\x0a"):
        if oid in attrs:
            return attrs[oid]
    return "?"


def parse_cert(der):
    _, cs, ce, _ = der_read(der, 0)  # Certificate
    _, ts, te, _ = der_read(der, cs)  # tbsCertificate
    i = ts
    tag, _, _, nxt = der_read(der, i)
    if tag == 0xA0:  # version
        i = nxt
    _, _, _, i = der_read(der, i)  # serial
    _, _, _, i = der_read(der, i)  # signature algorithm
    _, _, _, i = der_read(der, i)  # issuer
    _, vs, ve, i = der_read(der, i)  # validity
    t1, s1, e1, j = der_read(der, vs)
    t2, s2, e2, _ = der_read(der, j)
    not_after = parse_time(t2, der[s2:e2])
    subj_start = i
    _, _, _, i = der_read(der, i)
    subject = der[subj_start:i]
    spki_start = i
    _, _, _, i = der_read(der, i)
    spki = der[spki_start:i]
    return subject, spki, not_after


def read_pem(text):
    out = []
    cur = None
    for line in text.splitlines():
        line = line.strip()
        if line == "-----BEGIN CERTIFICATE-----":
            cur = []
        elif line == "-----END CERTIFICATE-----":
            if cur is not None:
                out.append(base64.b64decode("".join(cur), validate=True))
            cur = None
        elif cur is not None:
            cur.append(line)
    return out


def rust_bytes(b):
    s = []
    for c in b:
        # Spaces are escaped too: a line continuation would swallow them.
        if 0x20 < c < 0x7F and c not in (0x22, 0x5C):
            s.append(chr(c))
        else:
            s.append("\\x%02x" % c)
    # Keep the lines a sensible length.
    text = "".join(s)
    out = []
    while len(text) > 96:
        cut = 96
        # Don't split an escape.
        k = text.rfind("\\", cut - 3, cut)
        if k != -1:
            cut = k
        out.append(text[:cut])
        text = text[cut:]
    out.append(text)
    return 'b"' + "\\\n        ".join(out) + '"'


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("inputs", nargs="+")
    ap.add_argument("-o", "--output")
    ap.add_argument("--source", help="where the bundle came from, for the generated file's header")
    ap.add_argument("--keep-expired", action="store_true")
    args = ap.parse_args()

    files = []
    for p in args.inputs:
        if os.path.isdir(p):
            for root, _, names in os.walk(p):
                files += [os.path.join(root, n) for n in names if n.endswith((".crt", ".pem"))]
        else:
            files.append(p)
    files.sort()

    digest = hashlib.sha256()
    certs = []
    for f in files:
        with open(f, "rb") as fh:
            data = fh.read()
        digest.update(data)
        certs += read_pem(data.decode("ascii", "replace"))

    now = int(time.time())
    seen = set()
    roots = []
    skipped = 0
    for der in certs:
        try:
            subject, spki, not_after = parse_cert(der)
        except (ValueError, IndexError) as e:
            print("skipping a certificate that doesn't parse: %s" % e, file=sys.stderr)
            skipped += 1
            continue
        if not_after < now and not args.keep_expired:
            skipped += 1
            continue
        key = (subject, spki)
        if key in seen:
            continue
        seen.add(key)
        roots.append((common_name(subject), subject, spki, not_after))
    roots.sort(key=lambda r: (r[0].lower(), r[1]))

    src = args.source or ", ".join(args.inputs)
    lines = [
        "// Generated by tools/mkroots.py - do not edit. See docs/BROWSER.md (Network and TLS).",
        "// Source: %s" % src,
        "// Input SHA-256: %s; %d roots; generated %s." % (digest.hexdigest(), len(roots), time.strftime("%Y-%m-%d", time.gmtime(now))),
        "",
        "/// (name, subject DER, SubjectPublicKeyInfo DER, notAfter as Unix seconds)",
        "#[rustfmt::skip]",
        "pub static ROOTS: &[(&str, &[u8], &[u8], i64)] = &[",
    ]
    for name, subject, spki, na in roots:
        safe = name.replace("\\", "\\\\").replace('"', '\\"')
        lines.append("    (\"%s\"," % safe)
        lines.append("        %s," % rust_bytes(subject))
        lines.append("        %s," % rust_bytes(spki))
        lines.append("        %d)," % na)
    lines.append("];")
    text = "\n".join(lines) + "\n"
    if args.output:
        with open(args.output, "w") as fh:
            fh.write(text)
    else:
        sys.stdout.write(text)
    print("%d roots written (%d skipped)" % (len(roots), skipped), file=sys.stderr)


if __name__ == "__main__":
    main()
