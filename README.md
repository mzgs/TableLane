# TableLane
Navigate Your Data

Double-click a saved MariaDB connection in the left sidebar, or select it and press Enter, to connect. A green dot marks a successful connection; expand its databases to see tables. Connections use SQLx, with only MariaDB enabled for now. The dot reflects the last successful connection; background health checks are not yet implemented.

Double-click a table, or select it and press Enter, to show its data in the content area. The preview uses virtual scrolling and 1,000-row pages, with resizable columns. Previous and Next browse the matching results; Next is disabled on the last page. Refresh reloads the current page. NULL values are marked explicitly; binary values appear as hexadecimal text. Open another table to replace the preview, or reopen the current table to start over.

Click a column header to cycle through ascending, descending, and default ordering. Sorting runs in MariaDB on the original column values, with primary-key columns used to break ties. Tables without a primary key use all columns as the default ordering. Filtering or sorting returns to the first page.

Choose a filter column, enter text, and press Enter or Apply to match values containing that literal text. Filters match full values, including text beyond the truncated preview; binary columns match hexadecimal text. Apply filters to additional columns to combine them with AND. Selecting a column shows its current filter; applying an empty value removes that column's filter. Clear filters removes all filters. NULL values do not match text filters. Filter matching follows MariaDB's collation rules.

Save or cancel row edits before filtering, sorting, refreshing, or changing pages. Pages are live queries rather than a snapshot: concurrent inserts, deletes, or changes to sort values can shift rows between pages. Large offsets, substring filters, and sorting tables without a primary key can be slow on large tables.

Cell previews fetch at most 200 text characters or 100 binary bytes, plus a small lookahead to detect truncation. Longer values end with `…`. Selecting a row opens the right sidebar and loads its full values for editing. A single `Edited` status appears at the top when values change; press Cmd+S to save to MariaDB or Escape to discard the draft. Save and Cancel buttons provide the same actions. Unsaved edits must be saved or cancelled before switching rows or tables.

Saving requires an InnoDB table with a primary key. Saves check the original row for concurrent changes and roll back failed or lossy conversions. Generated, spatial, and bit columns cannot be edited; binary fields use hexadecimal text. Untouched NULL values are preserved, while typing into a NULL field makes it a text value.

Live MariaDB tests require an isolated server containing `tablelane_test.widgets` and an empty `empty_db` database, with a passwordless `root` account. Run them with `TABLELANE_TEST_MARIADB_PORT=<port> cargo test --locked mariadb_ -- --ignored`.

Connection passwords are encrypted with AES-256-GCM in settings JSON and settings exports, using a fresh random nonce for each save. Older plaintext passwords still load and are encrypted on the next save.

The encryption key is embedded in `src/settings/password.rs` and can be recovered from the app. Keep it stable: changing it makes existing encrypted passwords unreadable. Use an OS keychain if stronger credential protection is needed.
