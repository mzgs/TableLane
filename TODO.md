# TableLane feature progress

Updated: 2026-10-12. Continue from this file in later sessions.

## Scope

Implement the missing features identified in the app review. OS keychain storage
is excluded at the user's request. Keep existing encrypted settings compatible.
Use existing GPUI Kit components and dependencies; preserve validation and
protection against unsaved edits and concurrent database changes.

## Checklist

- [ ] Connection management
  - [x] Edit and delete saved connections
  - [x] Test connection before saving
  - [ ] Disconnect/reconnect, metadata refresh, accurate connection health
- [ ] Data transfer
  - [x] CSV/JSON export of full table/query values (never truncated previews)
  - [ ] CSV import with column mapping, validation, and transaction rollback
- [ ] Row operations
  - [ ] Insert, duplicate, delete with appropriate confirmation
  - [ ] Bulk actions with stable row identity
  - [x] Explicit NULL control, distinct from empty strings
- [ ] Workspace recovery
  - [x] Persist and restore SQL drafts and tabs
  - [ ] Persist table filters, sorting, and pagination
  - [ ] Protect unsaved work when quitting
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

## Continuation notes

- Next batch: table workspace state and full quit protection for row edits and
  running database operations. Keep every incomplete checkbox above visible until
  verified.
- The full roadmap is not complete.
- Edited connections apply to future sessions; existing tabs retain their old
  session and label. Deletion similarly keeps existing tabs usable. Any future
  disconnect/reconnect workflow must handle these retained sessions explicitly.
- Regression checks are in `src/connection_dialog.rs`, `src/table_view.rs`, and
  `src/sql_editor.rs`. Live tests require an isolated MariaDB instance; use the
  fixture and invocation documented in README (serial execution avoids shared
  fixture interference).
- Credentials live in `src/settings.rs` and `src/settings/password.rs`.
- Connections are retained in `src/connections.rs`; open tabs hold session Arcs.
  Disconnect/reconnect must account for these tabs and unsaved/running work.
- `src/database.rs` shares one mutex-protected MySQL connection per session.
  Cancellation must stop the server query, not only abandon the UI task.
- Table previews and SQL results truncate values. Table export fetches full values
  in one streaming SELECT; SQL export reads the private temporary snapshot from
  the completed execution and must never rerun the statement.
- SQL recovery lives in `src/workspace_state.rs`; the app shell owns autosave and
  restoration, SQL editors hold offline drafts, and connected sessions retain a
  credential-free connection identity. Preserve offline text when extending table
  recovery or disconnect/reconnect.
- README documents the current behavior and test-server setup.
- Run `cargo fmt --check`, `cargo test --locked`, and Clippy after each batch.
  Live MariaDB tests are ignored unless an isolated test server is configured.
