# SPDX-License-Identifier: Apache-2.0
"""Shared boot evidence and paired OVMF discovery for acceptance harnesses."""

from __future__ import annotations

import atexit
import os
import re
import shutil
import tempfile
from pathlib import Path

# QEMU rejects `unix:` socket paths of this length or more (SUN_PATH_MAX),
# which breaks any checkout whose own path is longer than about 80 characters.
UNIX_SOCKET_PATH_LIMIT = 108

_socket_dir: Path | None = None


def socket_path(name: str) -> Path:
    """A short-lived Unix socket path that fits QEMU's length limit from any checkout.

    Unix socket paths must be less than 108 bytes (SUN_PATH_MAX), but a
    `<checkout>/build/.../name` path computed from a long checkout directory
    exceeds that, so QEMU aborts with `UNIX socket path ... is too long`.
    Socket files therefore live in a process-private directory under /tmp,
    removed at exit. Logs, screenshots, and other artifacts stay in build/.
    """
    global _socket_dir
    if _socket_dir is None:
        _socket_dir = Path(tempfile.mkdtemp(prefix="aeth-sock-", dir="/tmp"))
        atexit.register(shutil.rmtree, _socket_dir, ignore_errors=True)
    path = _socket_dir / name
    length = len(os.fsencode(path))
    if length >= UNIX_SOCKET_PATH_LIMIT:
        raise RuntimeError(
            f"Unix socket path too long: {path} is {length} bytes, "
            f"but QEMU requires paths of fewer than {UNIX_SOCKET_PATH_LIMIT} bytes (SUN_PATH_MAX)"
        )
    return path

BASE_MARKERS = (
    "AETHER_MM_OK", "AETHER_USERCOPY_FIXUP_OK", "AETHER_SCHED_OK",
    "AETHER_ISOLATION_OK", "AETHER_CAP_OK",
    "AETHER_IPC_OK", "AETHER_IPC_USER_OK", "AETHER_APP_OK", "AETHER_FS_OK",
    "AETHER_SANDBOX_OK", "AETHER_SUPERVISOR_OK", "AETHER_RECOVERY_OK",
    "AETHER_REVOCATION_OK", "AETHER_PACKAGE_OK", "AETHER_RUNTIME_OK",
    "AETHER_UPDATE_OK", "AETHER_GRAPHICS_OK", "AETHER_DESKTOP_OK", "AETHER_FILES_OK",
    "AETHER_FILES_MUTATE_OK", "AETHER_WM_OK", "AETHER_POINTER_OK",
    "AETHER_PORTAL_CONSENT_OK", "AETHER_LINUX_ROOT_OK",
    "AETHER_LINUX_IMPORT_OK", "AETHER_LINUX_OVERLAY_OK", "AETHER_LINUX_VMA_OK",
    "AETHER_RUNTIME_ROOT_OK", "AETHER_LINUXD_OK",
    "AETHER_LINUXD_DENY_OK", "AETHER_LINUXD_USER_OK",
    "AETHER_LINUX_APPIMAGE_OK", "AETHER_LINUX_APPIMAGE_EXEC_OK",
    "AETHER_LINUX_APPIMAGE_STAGED_OK",
    "AETHER_LINUX_OVERLAY_PERSIST_OK", "AETHER_LINUX_OVERLAY_ERASE_OK",
    "AETHER_LINUX_DEMAND_PAGING_OK",
    "AETHER_LINUX_FILEMETA_OK", "AETHER_LINUX_DIR_OK",
    "AETHER_LINUX_PIPE_OK", "AETHER_LINUX_UNIX_OK", "AETHER_LINUX_SECCOMP_OK", "AETHER_LINUX_CLONE_PROCESS_OK", "AETHER_LINUX_COW_FORK_OK", "AETHER_LINUX_EVENT_OK", "AETHER_LINUX_INOTIFY_OK",
    "AETHER_LINUX_POLL_BLOCK_OK", "AETHER_LINUX_EPOLL_BLOCK_OK",
    "AETHER_LINUX_PIPE_BLOCK_OK", "AETHER_LINUX_NANOSLEEP_OK",
    "AETHER_LINUX_TIMERFD_OK",
    "AETHER_LINUX_INOTIFY_BLOCK_OK", "AETHER_LINUX_PROC_OK", "AETHER_LINUX_DEV_OK",
    "AETHER_LINUX_SYSCALL_POLICY_OK", "AETHER_LINUX_QUOTA_OK", "AETHER_LINUX_DENIAL_OK",
    "AETHER_LINUX_THREAD_OK", "AETHER_LINUX_SHARED_MMAP_OK",
    "AETHER_LINUX_SHARED_FORK_OK", "AETHER_LINUX_GUARD_OK",
    "AETHER_LINUX_MAP_SHARED_FILE_OK",
    "AETHER_LINUX_MREMAP_OK",
    "AETHER_LINUX_FUTEX_TIMEOUT_OK",
    "AETHER_LINUX_SIGNAL_OK", "AETHER_LINUX_CROSS_PROCESS_SIGNAL_OK",
    "AETHER_LINUX_SIGINFO_OK", "AETHER_LINUX_ALTSTACK_OK",
    "AETHER_LINUX_RESTART_OK", "AETHER_LINUX_SIGNAL_DEFAULT_OK",
    "AETHER_LINUX_ROBUST_FUTEX_OK", "AETHER_LINUX_CLONE3_OK",
    "AETHER_LINUX_RSEQ_OK",
    "AETHER_LINUX_FORK_OK",
    "AETHER_LINUX_PROCESS_GROUP_OK",
    "AETHER_LINUX_PROCESS_GROUP_KILL_OK",
    "AETHER_LINUX_EXECVE_OK",
    "AETHER_LINUX_EXECVE_DYN_OK",
    "AETHER_LINUX_DYN_INTERP_OK", "AETHER_LINUX_DYN_MAIN_OK",
    "AETHER_LINUX_DYN_OK", "AETHER_LINUX_OK",
    "AETHER_INPUT_OK",
    "AETHER_GATE4_DISPLAY_CREATE_OK",
    "AETHER_GATE4_PRESENT_OK",
    "AETHER_GATE4_INPUT_OK",
    "AETHER_GATE4_GUEST_DISPLAY_OK",
    "AETHER_PORTAL_OK",
    "AETHER_SHELL_OK", "AETHER_BOOT_OK",
)
FAILURE = re.compile(r"\bAETHER_[A-Z0-9_]*(?:FAILED|PANIC)\b")


def has_failure(text: str) -> bool:
    """Reject subsystem failures even when a later global success marker exists."""
    return FAILURE.search(text) is not None


def find_firmware() -> tuple[Path, Path | None]:
    code_env = os.environ.get("OVMF_CODE")
    vars_env = os.environ.get("OVMF_VARS")
    if vars_env and not code_env:
        raise SystemExit("error: OVMF_VARS requires OVMF_CODE")
    if code_env:
        code = Path(code_env)
        variables = Path(vars_env) if vars_env else None
        for path in (code, variables):
            if path is not None and not path.is_file():
                raise SystemExit(f"error: firmware not found: {path}")
        return code, variables
    # Discover *pairs*, never independently choose mismatched 2 MiB/4 MiB files.
    for directory in ("/usr/share/OVMF", "/usr/share/edk2/ovmf"):
        for suffix in ("_4M", ""):
            code = Path(directory) / f"OVMF_CODE{suffix}.fd"
            variables = Path(directory) / f"OVMF_VARS{suffix}.fd"
            if code.is_file() and variables.is_file():
                return code, variables
    monolithic = Path("/usr/share/qemu/OVMF.fd")
    if monolithic.is_file():
        return monolithic, None
    raise SystemExit("error: OVMF firmware pair not found; set OVMF_CODE and OVMF_VARS")


def find_iso(build: Path, override: str | None = None) -> Path:
    if override:
        image = Path(override).resolve()
    else:
        images = [path for path in (build / "ISOs").glob("*.iso") if path.is_file()]
        if not images:
            raise ValueError(f"no ISO under {build / 'ISOs'}; run `make iso`")
        image = max(images, key=lambda path: (path.stat().st_mtime_ns, path.name)).resolve()
    if not image.is_file() or image.stat().st_size == 0:
        raise ValueError(f"ISO is missing or empty: {image}")
    return image
