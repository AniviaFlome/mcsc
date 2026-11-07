# AGENTS.md — mcsc (Minecraft Server World Converter)

Rust workspace: `mcsc-core` (pure filesystem+NBT logic, no GUI dep) + `mcsc-gui`
(`src/main.rs` entry + `AppSpec`, `src/state.rs` state + domain helpers,
`src/view.rs` Rune view + rune-kit builders, `src/workers.rs` background
scan/convert threads). Rune + rune-kit pinned to a `dev` rev in
`mcsc-gui/Cargo.toml`; Catppuccin Mocha/Latte via `Kit::from_palette`.

## Commands (always via nix-shell; bare `cargo` fails with `linker cc not found`)

```sh
nix-shell --run 'cargo fmt; cargo test --workspace; cargo clippy --workspace -- -D warnings; cargo fmt --check'
nix-shell --run 'cargo build -p mcsc-gui && cargo run -p mcsc-gui'  # binary: target/debug/mcsc
```

`ConvertOptions`/`ScanOptions` gain a field → fix every struct literal in
`mcsc-core/src/convert.rs` tests too (else E0063). New modules must be
declared + re-exported in `mcsc-core/src/lib.rs`.

## Architecture notes

- Long work (scan, convert, backup copy) runs in `std::thread` workers
  spawned from press handlers, reporting back via `AppHandle::update`
  (runs on the UI thread, commits next frame). Never block a handler:
  a stalled handler looks like a frozen UI with buttons that never respond.
- Live progress: `ProgressReporter` closures call `handle.update(|s,_| …)`
  to set `s.progress`; completion clears `busy`/`progress`, sets
  `banner` + reports. One task at a time by design (`is_busy` guard).
- File pickers use Rune's `Tx::open_folder` (`native-file-dialog`
  feature: XDG portal on Linux). The answer callback runs as a later
  action; headless answers it with a `--script` `file-dialog` event.
- Text inputs bind to `rune::TextEdit` fields via
  `TextInput::on_edit(|s| &mut s.field)`; `on_change` mirrors the old
  Iced field-changed side effects (clear `detection`, refresh output
  default, set `output_customized`).
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

## GUI QA (headless Rune, no Xvfb)

- Rebuild before snapshotting, and kill the old app first — a stale
  `target/debug/mcsc` is the #1 cause of "my change did nothing".
- Prefer headless over synthetic clicks:
  `target/debug/mcsc --snap out.png --size 1152x800 --scale 1`,
  `--scene light`, `--tree tree.json` (keyed elements, 122 layout nodes),
  `--script events.json` for clicks/keys/file-dialog answers.
  Unit headless tests use `Runtime::new(App, view, size, Env)` +
  `rt.find("key")` + `pointer_down/up` + `frame()`; keep keys stable
  (`server-root`, `browse`, `detect`, `scan`, `convert`, `theme`, …).
- Confirm via stderr `[mcsc] message:` lines (all actions log there except
  keystroke/progress noise) plus snapshots. One unverified click proves nothing.
- If clicks/keyboard suddenly stop registering while the process idles,
  suspect overlapping sessions or stale window IDs: `pkill -f target/debug/mcsc`.

## Debugging

- Handlers log every action as `[mcsc] message: <Name>` on stderr; run the
  GUI from a terminal to see it.
- Fixtures: unit tests build tiny worlds under `std::env::temp_dir()`;
  `/tmp/opencode/mcsc-qa/` holds throwaway QA fixtures (safe to delete).
- Real-world reference data (not in repo): `~/Downloads/world` (Paper modern
  26.2) and `~/Downloads/world-converted-vanilla` (converted copy).
