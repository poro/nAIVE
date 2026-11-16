## What and why


## How it was verified
- [ ] `cargo fmt --all -- --check`, workspace tests and clippy `-D warnings`
- [ ] Lua scene tests (`cd project && naive test`)
- [ ] Rendering changes: `capture_frame` compared with master (identical, or looked at)
- [ ] Performance-sensitive changes: `benchmark` before and after

Not checked:

## Review
- [ ] Under ~500 changed lines of code, or reviewed (a person, or `/code-review` with its findings answered here)
