#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Emit a VMware Workstation VM definition that boots the newest Aether ISO.
set -eu

cd "$(dirname "$0")/.."

exec "${PYTHON:-python3}" tools/make_vmware_vmx.py
