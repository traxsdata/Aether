# Notifications

`SYS_NOTIFY(title, title_len, msg, msg_len)` lets a windowed app post a short
message. The kernel, not the app, draws both the toast and the history panel,
and records the calling process's name. An app cannot claim another app's
identity or cover the panel with its own pixels.

- **Capability:** `CAP_WINDOW`.
- **Limits:** title 63 bytes, message 127 bytes; control characters become
  `?`. Four notifications per app in any ten-second window; further calls
  return `-E_LIMIT`. Kernel messages share the same history but have their own
  rate bucket.
- **History:** the last 16 notifications, newest first. **Super+N** or the
  bell at the right end of the menu bar opens the panel; **Clear** empties it.
  Toasts still disappear after six seconds.
- **Privacy:** notifications are not persisted and are cleared when the
  desktop locks.
