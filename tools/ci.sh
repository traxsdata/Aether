#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Local pull-request CI gate. Keeping this script runnable makes failures
# reproducible without CI; a hosted workflow is a separate follow-up.
set -eu

cd "$(dirname "$0")/.."

make lint
make docs-check
make license-check
make unittest
make compat-test
make appimage-test
make squashfs-test
make update-test
make test
make large-apps-test
make iso-test
make vmware-vmx
make shell-test
make fault-test
make crash-screen-test
make rollback-test
make negative-test
make release-demo
