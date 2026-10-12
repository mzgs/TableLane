# TableLane feature progress

Updated: 2026-10-12. Continue from this file in later sessions.

## Scope

Implement the missing features identified in the app review. OS keychain storage
is excluded at the user's request. Keep existing encrypted settings compatible.
Use existing GPUI Kit components and dependencies; preserve validation and
protection against unsaved edits and concurrent database changes.

## Checklist

- [x] Connection management
  - [x] Edit and delete saved connections
  - [x] Test connection before saving
  - [x] Disconnect/reconnect, metadata refresh, accurate connection health
- [ ] Data transfer
  - [x] CSV/JSON export of full table/query values (never truncated previews)
  - [ ] CSV import with column mapping, validation, and transaction rollback
- [ ] Row operations
  - [ ] Insert, duplicate, delete with appropriate confirmation
  - [ ] Bulk actions with stable row identity
  - [x] Explicit NULL control, distinct from empty strings
- [x] Workspace recovery
  - [x] Persist and restore SQL drafts and tabs
  - [x] Persist table filters, sorting, and pagination
  - [x] Protect unsaved work when quitting
- [ ] Query cancellation without leaving a busy/broken shared session
- [ ] SQL workflow
  - [x] Run selected SQL
  - [ ] Multi-statement scripts and multiple result sets
  - [ ] Saved queries
  - [ ] Browse/export results beyond 1,000 rows (export complete; browsing still capped)
- [ ] Connection security: SSH tunneling and configurable TLS certificates
- [ ] Database support: MySQL, PostgreSQL, SQLite, MongoDB
- [ ] Navigation: sidebar search; views, triggers, procedures, functions
- [ ] Schema controls: create/drop tables and foreign-key editing

## Progress log

### 2026-10-12 — Started

- Read the existing flows and GPUI Kit coding/design guides.
- Existing features include MariaDB browsing, filtering, sorting, transactional
  row updates, schema column/index changes, and persistent SQL history.
- Completed first implementation batch:
  - Select a connection (or one of its database/table children), then use the
    sidebar's Edit connection… button. The existing form loads all saved fields,
    preserves passwords, and replaces the saved entry rather than duplicating it.
  - Delete… in that form shows a confirmation. Failed writes preserve the saved
    connection and show an error; stale forms cannot overwrite newer settings.
    Database data and existing tabs are retained.
  - Test connection checks connectivity and metadata access without saving.
    Submission is disabled during the test; changed input invalidates results.
    Unsupported databases report the existing driver limitation.
  - Row fields expose Use NULL. Text editing is disabled while selected;
    unchecking restores the text draft. Empty text and SQL NULL remain distinct.
  - Run and Cmd/Ctrl+Enter execute the selection if present, otherwise the whole
    statement. History records the executed SQL.
- Updated README to describe the new controls and session behavior.
- Verification:
  - `cargo test --locked`: 25 passed; 11 server-dependent tests ignored.
  - On a temporary isolated MariaDB server: 9 existing live database/UI tests
    passed, including NULL/empty-string save round trips and selected-SQL
    execution; the new connection-test success/invalidation check also passed.
  - `cargo clippy --locked --all-targets -- -D warnings`: passed.
  - `cargo fmt --check` and `git diff --check`: passed.
  - Temporary server shut down after testing; saved user connections untouched.
- Remaining checks: manual native-window review, theme/zoom inspection. The
  existing debug-startup integration test was not run in this batch.
- No dependencies added, no keychain changes, no commit created.

### 2026-10-12 — Full-value exports

- Added Export CSV… / Export JSON… to table and SQL result controls, using native
  save pickers with cancellation, progress, duplicate-submission guards, and
  inline errors. Unsaved row edits block table export.
- Table exports stream every matching row using the applied filters and sort
  order, independent of the current page. Shared metadata and query builders
  keep export and browsing validation consistent.
- SQL execution captures the full first result set in a private temporary file;
  exports read that completed snapshot without rerunning SQL. The UI keeps its
  bounded 1,000-row/value previews and now reports the full row count.
- CSV preserves quoted strings, empty text, full hex binary values, and unquoted
  `\N` for NULL. JSON uses columns/rows arrays so duplicate column names and
  exact string values survive. Exports replace their destination only after
  writing and flushing the whole file; errors preserve existing files.
- Temporary files are removed on normal result/tab disposal. Crash cleanup and
  result-set browsing beyond 1,000 rows remain unimplemented; large results need
  local disk space. README documents the formats and snapshot lifecycle.
- Verification:
  - `cargo test --locked`: 26 passed; 12 server-dependent tests ignored.
  - Temporary isolated MariaDB: all 11 `mariadb_` database/UI tests passed,
    including new full-value export, native picker cancellation/error flows,
    1,005-row snapshots, duplicate columns, NULL/empty text, and a SQL counter
    proving exports never repeat execution. Filtered/sorted table export also
    covered 1,003 rows while ignoring a nonzero browse page.
  - `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check`,
    and `git diff --check`: passed.
  - Temporary server shut down after testing; saved user connections untouched.
- Remaining checks: manual native-window/theme/zoom review, existing debug-startup
  integration test. Crash recovery and cancellation stay on the roadmap.
- No dependencies added, no credential-storage changes, no commit created.

### 2026-10-12 — SQL draft and tab recovery

- SQL tabs autosave exact draft text, tab numbers/order, and the selected SQL tab
  to private `workspace.json` alongside settings. Edits and tab switching debounce
  for 300 ms; closing/discarding tabs and normal quit flush immediately.
- Tabs restore offline, remain editable without a saved connection, and never
  reconnect or execute SQL automatically. Opening a matching saved connection
  enables Run and completions only when its database is accessible. The identity
  excludes passwords and names; renamed connections can reattach safely.
- Invalid or oversized workspace files remain untouched and disable autosave;
  failed writes retain earlier recovery data and show errors with Retry autosave.
  App Quit and window close ask before losing SQL drafts after a failed save.
- Reused the existing atomic JSON writer and 1 MiB limit. Draft text is plain
  local JSON; results, undo history, cursor/selection, and table/Structure tabs are
  not restored. Crash/OS termination can lose changes since the last autosave.
- Full quit protection for unsaved row edits and running operations remains
  unchecked above; the GPUI final shutdown callback cannot veto OS termination.
- Verification:
  - `cargo test --locked -- --test-threads=1`: 28 passed; 13 server-dependent
    tests ignored. Recovery checks covered offline editing, debounce, tab/order
    restoration, discard/cancel, oversized/invalid files, failed-save retention,
    retry, and the native window-close callback.
  - Isolated MariaDB: all 12 `mariadb_` tests passed. The new reconnect check
    verified endpoint/user matching, renamed connections, inaccessible databases,
    new-tab persistence, and a SQL counter proving restore/connect never execute.
  - The existing debug-startup integration test also passed on an isolated
    `apps.apps` fixture. Temporary server shut down; saved user connections and
    production SQL history were untouched.
  - `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check`,
    and `git diff --check`: passed.
  - Parallel full-suite runs intermittently failed existing settings/connection
    UI assertions; each passed alone and the full serial run passed. Root cause
    remains undiagnosed; serial execution is the verified command for this batch.
- Remaining checks: manual native-window/theme/zoom review. Table/Structure
  restoration and full quit protection remain on the roadmap.
- No dependencies added, no credential-storage changes, no commit created.

### 2026-10-12 — Table recovery and quit protection

- Extended the existing workspace file to preserve SQL/table tab order and the
  selected recoverable tab, with applied table filters, sorting, and page.
  Existing SQL-only files remain readable; invalid files and failed writes keep
  the same preservation and retry behavior. Row drafts and results are excluded.
- Table tabs restore offline without connecting. Connecting a matching saved
  endpoint reloads accessible tables with their saved query; renamed connections
  update labels. Missing tables and unmatched connections remain offline.
  Removed filter/sort columns surface errors with Retry and Reset controls.
- Applied query changes autosave after 300 ms; opening, switching, closing, and
  quitting reuse the existing recovery lifecycle. Structure tabs remain excluded;
  if a Structure tab was selected, recovery selects the first saved tab.
- Quit and native window close inspect every tab, including inactive tabs. They
  block while connections, reads, row saves, schema changes, or exports run, and
  ask before discarding row edits or losing unsaved recovery data. Cancel keeps
  edits; confirming discard never writes the row draft to the database.
- Tab close now also waits for table/structure reads and exports. Outstanding
  row reads remain tracked even after selection changes invalidate their result.
- Verification:
  - `cargo test --locked -- --test-threads=1`: 30 passed; 14 live tests ignored.
    Added mixed offline recovery/native-close checks and legacy-file validation.
  - Isolated MariaDB: all 13 `mariadb_` tests passed, including new 1,005-row
    recovery coverage for restored filters, descending sort, page, renamed and
    unavailable connections/tables, stale columns, reset, and query autosave.
    Existing SQL tests also verified quit protection during an inactive query.
  - Debug-startup integration test passed with the isolated `apps.apps` fixture.
  - `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check`,
    and `git diff --check`: passed.
  - Temporary server shut down after testing; saved user connections and
    production SQL history untouched.
- Remaining checks: manual native-window/theme/zoom review. Crash/OS forced
  termination can still lose recent drafts or interrupt database work, and
  running work has no cancellation yet. Restored pages use current database data.
- No dependencies added, no credential-storage changes, no commit created.

### 2026-10-12 — Connection lifecycle and health

- Added selected-connection Connect, Disconnect…, Reconnect…, and Refresh metadata
  controls, available when a connection or any database/table child is selected.
  Removed children fall back to their connection root so recovery controls stay
  accessible. Metadata refresh preserves existing database expansion state.
- Disconnect/reconnect checks all tabs using that exact session, including
  inactive tabs, and blocks on row edits, reads, saves, queries, schema work, and
  exports. Confirmation explains rollback of uncommitted SQL transactions and
  loss of temporary tables/session variables. Cancel and stale confirmations
  leave sessions alone; guards are rechecked before closing the socket.
- SQL drafts and completed SQL export snapshots survive offline transitions.
  Table queries retain their applied filters, sorting, and page. Reconnection
  uses a new session, reloads available tables and Structure tabs, and never
  runs SQL drafts. Missing objects remain offline. Unexpected connection loss
  preserves dirty row drafts until the user cancels them before reconnecting.
- Edited/deleted saved connections retain their live sessions as “Previous
  session” sidebar entries. These can still browse, refresh metadata, or
  disconnect, but cannot reconnect using removed settings. Explicit disconnect
  does not immediately reattach tabs to another already-open matching session.
- Metadata refresh updates the tree and completions in existing SQL editors on
  the same session; failed refreshes preserve the previous metadata snapshot.
  Table and Structure previews keep their own Refresh commands.
- Idle sessions receive background pings about every 30 seconds, with a
  five-second timeout. Checks skip busy sessions and never cancel user queries.
  Failed checks and operation transport errors remove the connected indicator,
  disable unavailable commands, and require explicit reconnect.
- Verification:
  - `cargo test --locked -- --test-threads=1`: 30 passed; 16 live tests ignored.
  - Isolated MariaDB: all 15 `mariadb_` tests passed. New database/UI coverage
    includes metadata/completion refresh, unchanged SQL session variables,
    inactive-tab guards, confirmation cancellation/rechecks, offline draft and
    export retention, table/Structure reattachment, retained previous sessions,
    killed-server-session health detection, and double-click reconnect. A final
    targeted database run also verified uncommitted SQL rollback on disconnect
    and immediate health invalidation after an operation transport error.
  - Debug-startup integration test passed using isolated `apps.apps` fixtures.
  - `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check`,
    and `git diff --check`: passed.
  - One earlier live run timed out in an existing schema UI check; the final
    complete serial live run passed. New cross-view tests isolate SQL history.
  - Temporary servers shut down; saved user settings and SQL history untouched.
- Remaining checks: manual native-window/theme/zoom review. Health polling has a
  delay and skips long-running operations; there is no automatic reconnect or
  query cancellation. Reconnect drops database session state after confirmation.
- No dependencies added, no credential-storage changes, no commit created.

## Continuation notes

- Next batch: CSV import with column mapping, validation, and transaction
  rollback. Keep every incomplete checkbox above visible until verified.
- The full roadmap is not complete.
- Edited connections apply to future sessions; existing tabs retain their old
  session and label. Retained sessions remain controllable through “Previous
  session” sidebar entries, including after deletion of their saved settings.
- Regression checks are in `src/connection_dialog.rs`, `src/table_view.rs`, and
  `src/sql_editor.rs`. Live tests require an isolated MariaDB instance; use the
  fixture and invocation documented in README (serial execution avoids shared
  fixture interference).
- Credentials live in `src/settings.rs` and `src/settings/password.rs`.
- Connections and retained sessions live in `src/connections.rs`; open tabs hold
  session Arcs. The app shell guards lifecycle commands across every affected
  tab. Keep these guards when adding import or row operations.
- `src/database.rs` shares one mutex-protected MySQL connection per session.
  Cancellation must stop the server query, not only abandon the UI task.
- Table previews and SQL results truncate values. Table export fetches full values
  in one streaming SELECT; SQL export reads the private temporary snapshot from
  the completed execution and must never rerun the statement.
- SQL/table recovery lives in `src/workspace_state.rs`; the app shell owns
  autosave, restoration, and quit guards. SQL editors hold offline text and table
  views hold offline queries; sessions retain a credential-free identity. Preserve
  both when adding new operations, and include their running work in lifecycle
  and quit guards.
- README documents the current behavior and test-server setup.
- Run `cargo fmt --check`, `cargo test --locked`, and Clippy after each batch.
  Live MariaDB tests are ignored unless an isolated test server is configured.
