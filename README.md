# TableLane
Navigate Your Data

Double-click a saved MariaDB connection in the left sidebar, or select it and press Enter, to connect. A green dot marks a successful connection; expand its databases to see tables. Connections use SQLx, with only MariaDB enabled for now. The dot reflects the last successful connection; background health checks are not yet implemented.

Live MariaDB tests require an isolated server containing `tablelane_test.widgets` and an empty `empty_db` database, with a passwordless `root` account. Run them with `TABLELANE_TEST_MARIADB_PORT=<port> cargo test --locked mariadb_ -- --ignored`.

Connection passwords are encrypted with AES-256-GCM in settings JSON and settings exports, using a fresh random nonce for each save. Older plaintext passwords still load and are encrypted on the next save.

The encryption key is embedded in `src/settings/password.rs` and can be recovered from the app. Keep it stable: changing it makes existing encrypted passwords unreadable. Use an OS keychain if stronger credential protection is needed.
