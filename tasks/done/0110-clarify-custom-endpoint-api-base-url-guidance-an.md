Clarify custom endpoint API base URL guidance and validation (UX, AGENT, TUI)

## Outcome

- The custom endpoint prompt now identifies the value as an API root and shows `http://127.0.0.1:9090/v1` as the usual example.
- Root URLs with or without `/v1` remain valid, while complete `/chat`, `/chat/completions`, `/models`, and `/responses` operation URLs are rejected with corrective guidance.
- README and changelog guidance were updated; all 183 tests and Clippy pass.
