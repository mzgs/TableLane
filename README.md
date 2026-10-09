# TableLane
Navigate Your Data

Connection passwords are encrypted with AES-256-GCM in settings JSON and settings exports, using a fresh random nonce for each save. Older plaintext passwords still load and are encrypted on the next save.

The encryption key is embedded in `src/settings/password.rs` and can be recovered from the app. Keep it stable: changing it makes existing encrypted passwords unreadable. Use an OS keychain if stronger credential protection is needed.
