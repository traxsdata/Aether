#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Local equivalent of the pull-request CI gate. A GitHub Actions workflow can
# call the same commands once it is enabled; keeping this script runnable makes
# failures reproducible without CI.
set -eu

cd "$(dirname "$0")/.."

make lint
make docs-check
make license-check
make unittest
make test
make shell-test
make fault-test
make negative-test
