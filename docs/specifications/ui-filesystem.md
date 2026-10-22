# Unified filesystem and desktop experience

This note maps the proposed **AETHER-UX-002 — User Interface & Unified
Filesystem Specification** onto the current prototype and records what is
enforced today, what is only modelled, and what remains future work.

## Implemented policy

The host-testable `aether_services::ux` module defines the rules that must not
be duplicated by the desktop:

- stable `ResourceId` values that survive rename and move;
- explicit ownership: system, application-private, user, shared, external, and
  Trash;
- separate read, write, create, delete, rename, enumerate, and execute rights;
- file grants that never imply parent or sibling access;
- folder grants scoped to the selected subtree, including resources created
  later inside that subtree;
- one-time grants that are spent after a successful operation and persistent
  grants that remain until revoked;
- revocation, including bulk revocation during uninstall;
- search and metadata visibility filtered before results are emitted;
- per-application storage categories (`Data`, `Cache`, `Settings`,
  `Temporary`), quota accounting, safe cache cleanup, and uninstall policy;
- a bounded Recent list whose removal never deletes the underlying resource;
- the thirteen primary navigation locations, in the same order for ordinary
  browsing and file-selection portals.

Tests cover selected-file and selected-folder scope, cross-application vault
isolation, one-time versus persistent lifetime, revocation, search isolation,
identity preservation, cleanup, uninstall preservation, and navigation order.

## Kernel and desktop integration

- The RAM filesystem now has 64 inodes, with four reserved inode blocks, so the
  user workspace and private application categories fit without reusing the
  data area.
- The `home` volume seeds `Recent`, `Favorites`, `Documents`, `Downloads`,
  `Pictures`, `Music`, `Videos`, `Projects`, `Shared`, and `Trash`.
- `Applications` and `Devices` are service-backed locations under `/system`;
  they are not a route into `/apps`.
- Installed applications receive `/apps/<package-id>/private` with `Data`,
  `Cache`, `Settings`, and `Temporary` categories. The package identity, not
  the display name, owns the storage.
- Files presents the same thirteen locations in the specification order.
  Application-private vaults remain denied by the service adapter.
- Deleting from a user location moves the item into `Trash` instead of
  destroying it. Deleting from `Trash` is the explicit permanent action.
  Application-private cleanup is a separate service operation and does not use
  the user Trash.
- The existing portal keeps exact-file grants, bounded staging files, and
  atomic commit on save. Revocation blocks future portal opens; an issued file
  capability remains valid until closed, because bytes already read cannot be
  recalled.

## Not yet implemented

The following parts of the specification are documented as future work rather
than silently claimed:

- a separately isolated user-space `fsd`, `portald`, `securityd`, `indexd`, and
  compositor; the current adapters still include trusted ring-0 code;
- folder grants in the live kernel portal (the policy model supports them, but
  the syscall and on-disk grant record still carry exact files only);
- restoring an item from Trash and showing its original location;
- application-contributed Recent, Favorites, and collections;
- Open With, Share, drag-and-drop capability transfer, and clipboard grants;
- persistent preferences, full search indexing, version history, and durable
  storage across power loss;
- a screen reader, text scaling, IME support, and independently reviewed
  accessibility conformance.

These limitations match the existing architecture documents and must not be
read as completed isolation or a production filesystem.