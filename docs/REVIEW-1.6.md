# Review of 1.6: notifications

Based on `v1.5`. This finishes the notification item deferred from 1.3.

## Changes

- `SYS_NOTIFY` (62) accepts a bounded title and message from any app holding
  `CAP_WINDOW`. The kernel substitutes control characters, records
  `current->name`, and never accepts an app-supplied identity.
- Per-app rate limit: four notifications per ten-second window. Excess calls
  return `-E_LIMIT` and are logged; the kernel's own notifications use a
  separate bucket.
- A 16-entry in-memory history. **Super+N** or the bell at the right of the
  menu bar opens a kernel-drawn panel with the newest first and a Clear
  button. Toasts remain the transient view.
- History and the open panel are cleared by `wm_notifications_clear`; lock
  clears it through the same path.
- Kernel test: four accepted notifications, the fifth refused, history count
  and clear.

## Evidence

```text
make            PASS
make unittest   59 kernel / 71 terminal, 0 failed
```

The UI was checked by booting the ISO and using Super+N; no screenshot is
committed for this small panel.

## Not done

- Notification persistence across reboots and per-app mute controls.
- Action buttons, replies, and click-through to the posting window.
- A settings page for notification permissions; `CAP_WINDOW` is the gate for
  now.
