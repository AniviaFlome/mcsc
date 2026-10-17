# AGENTS.md — mcsc (Minecraft Server World Converter)

Rust workspace: `mcsc-core` (pure filesystem+NBT logic, no GUI dep) + `mcsc-gui`
(single `src/main.rs`, Iced 0.14 Elm-style `update`/`view`/`subscription`).
No git repo here; `Cargo.lock` is gitignored. `shell.nix` comment still says
"Iced 0.13" — stale; trust `mcsc-gui/Cargo.toml` (0.14).

## Commands (always via nix-shell; bare `cargo` fails with `linker cc not found`)

```sh
nix-shell --run 'cargo fmt; cargo test --workspace; cargo clippy --workspace -- -D warnings; cargo fmt --check'
nix-shell --run 'cargo build -p mcsc-gui && cargo run -p mcsc-gui'  # binary: target/debug/mcsc
```

`ConvertOptions`/`ScanOptions` gain a field → fix every struct literal in
`mcsc-core/src/convert.rs` tests too (else E0063). New modules must be
declared + re-exported in `mcsc-core/src/lib.rs`.

## Architecture notes

- Long work (scan, convert, backup copy) must run in
  `tokio::task::spawn_blocking` inside `Task::perform`; blocking the async
  executor freezes the UI with buttons that never respond.
- Live progress: worker threads send `Progress` over a channel held in the
  `PROGRESS_RX` global; `drain_progress` subscription is active only while
  `busy` is set. One task at a time by design.
- Conversion semantics that look like bugs but aren't: copy mode replaces
  stale files *inside the fresh copy* (forced overwrite; source is the safety
  net); dry-run predicts post-copy collisions via counterpart check
  (`dst_conflicts`); Paper keeps the seed outside `level.dat`
  (`world_gen_settings.dat` → injected as `Data/WorldGenSettings`), and
  offline servers use v3 UUIDs vs online v4 (`players::offline_uuid`).
  Detection deliberately ignores Paper-only leftovers (`paper-world.yml`,
  `data/paper/`) so converted worlds re-detect as Vanilla.
- Domain truth table lives in `README.md` (Layouts section) — keep it in
  sync when move logic changes.

## GUI QA (headless Xvfb via `agent-workspace-linux` skill)

- Rebuild before launching, and kill the old app first — a stale
  `target/debug/mcsc` is the #1 cause of "my change did nothing".
- Synthetic clicks are lossy under software rendering (press+release can land
  in one frame and iced drops it): retry clicks in move-then-click loops and
  confirm via stderr `[mcsc] message:` lines (all actions log there except
  keystroke/progress noise) plus screenshots. One unverified click proves nothing.
- If clicks/keyboard suddenly stop registering while the process idles,
  suspect overlapping sessions or stale window IDs: `pkill -f target/debug/mcsc`,
  stop the workspace, start fresh.

## Debugging

- `update()` logs every action as `[mcsc] message: <Name>` on stderr; run the
  GUI from a terminal to see it.
- Fixtures: unit tests build tiny worlds under `std::env::temp_dir()`;
  `/tmp/opencode/mcsc-qa/` holds throwaway QA fixtures (safe to delete).
- Real-world reference data (not in repo): `~/Downloads/world` (Paper modern
  26.2) and `~/Downloads/world-converted-vanilla` (converted copy).
