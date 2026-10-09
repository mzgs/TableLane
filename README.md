# TableLane
Navigate Your Data

Double-click a saved MariaDB connection in the left sidebar, or select it and press Enter, to connect. A green dot marks a successful connection; expand its databases to see tables. Connections use SQLx, with only MariaDB enabled for now. The dot reflects the last successful connection; background health checks are not yet implemented.

Double-click a table, or select it and press Enter, to show its data in the content area. The readonly preview uses virtual scrolling and shows up to 1,000 rows, with resizable columns. NULL values are marked explicitly; binary values appear as hexadecimal text. Open another table to replace the preview, or reopen the current table to reload it.

Cell previews fetch at most 200 text characters or 100 binary bytes, plus a small lookahead to detect truncation. Longer values end with `…`. Selecting a row opens the right sidebar and loads its full values for editing. A single `Edited` status appears at the top when values change; press Cmd+S to save to MariaDB or Escape to discard the draft. Save and Cancel buttons provide the same actions. Unsaved edits must be saved or cancelled before switching rows or tables.

Saving requires an InnoDB table with a primary key. Saves check the original row for concurrent changes and roll back failed or lossy conversions. Generated, spatial, and bit columns cannot be edited; binary fields use hexadecimal text. Untouched NULL values are preserved, while typing into a NULL field makes it a text value.

Live MariaDB tests require an isolated server containing `tablelane_test.widgets` and an empty `empty_db` database, with a passwordless `root` account. Run them with `TABLELANE_TEST_MARIADB_PORT=<port> cargo test --locked mariadb_ -- --ignored`.

Connection passwords are encrypted with AES-256-GCM in settings JSON and settings exports, using a fresh random nonce for each save. Older plaintext passwords still load and are encrypted on the next save.

The encryption key is embedded in `src/settings/password.rs` and can be recovered from the app. Keep it stable: changing it makes existing encrypted passwords unreadable. Use an OS keychain if stronger credential protection is needed.
