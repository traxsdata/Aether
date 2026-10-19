#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Local equivalent of the pull-request CI gate. The GitHub Actions workflow
# calls the same commands; keeping this script runnable makes failures
# reproducible without CI.
set -eu

cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo test --locked
make test
