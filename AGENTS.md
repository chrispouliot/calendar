# AGENTS.md

## Project overview

This project is a Rust/GTK4/Libadwaita calendar application. The goal is to build a GNOME Calendar-like UI and user experience while using this app's own backend for local storage, CalDAV sync, and reminders instead of Evolution Data Server/EDS.

Planning and GNOME Calendar research are captured in `plan.md`. GNOME Calendar was cloned for reference to:

```text
/tmp/gnome-calendar
```

Use that clone as read-only reference material for UI structure, Blueprint patterns, and behavior. Do not copy GNOME Calendar's EDS backend directly.

## Current scaffold

- Rust 2024 crate named `calendar`
- GTK dependency: Cargo package `gtk4`, imported in Rust as `gtk`
- Libadwaita dependency: Cargo package `libadwaita`, imported in Rust as `adw`
- Nix development shell in `flake.nix`
- Blueprint compiler is available in the dev shell

## Common commands

Run these from the repository root.

### Check

```sh
cargo check
```

### Build

```sh
cargo build
```

### Run

```sh
cargo run
```

### Tests

```sh
cargo test
```

### Clippy

```sh
cargo clippy --all-targets --all-features -- -D warnings
```

### Formatting

```sh
cargo fmt --all
```

### Nix development shell

If needed, enter the project environment with:

```sh
nix develop
```

The shell provides Rust, GTK4, Libadwaita, pkg-config, and `blueprint-compiler`.

For automated or non-interactive commands that execute Rust binaries (including
`cargo test` and `cargo run`):

- Inside the coding-agent sandbox, run Cargo commands directly. The sandbox is
  already built from this development shell and does not include the `nix` command.
- Outside the sandbox, prefer `nix develop -c <command>`.

A bare host shell may compile successfully via `pkg-config` but fail at runtime
because Nix-store GLib/GTK libraries are absent from `LD_LIBRARY_PATH`. Do not
persist a global `LD_LIBRARY_PATH`; the development shell and sandbox image provide
their own runtime environments.

## Development guidance

- Keep UI and backend boundaries separate.
- Use GTK/Libadwaita and gtk-rs idioms rather than mechanically porting C code.
- Use GNOME Calendar as a behavioral and visual reference, not as backend architecture.
- Prefer Blueprint templates and GResources for UI once the initial app shell is in place.
- Use tests for pure logic such as date calculations, range queries, recurrence handling, repository behavior, CalDAV parsing, and reminder scheduling.
- Visual GTK work may require manual verification with `cargo run` in addition to `cargo check`.
- Avoid introducing EDS/Evolution dependencies; this app should own its calendar, sync, and reminder backend.

## Important reference files

Local plan:

```text
plan.md
```

GNOME Calendar reference paths:

```text
/tmp/gnome-calendar/src/gui/gcal-window.blp
/tmp/gnome-calendar/src/gui/gcal-window.c
/tmp/gnome-calendar/src/gui/views/
/tmp/gnome-calendar/src/gui/common/
/tmp/gnome-calendar/src/gui/event-editor/
/tmp/gnome-calendar/src/gui/calendar-management/
/tmp/gnome-calendar/src/core/
```

## Suggested first milestone

Build the GTK/Libadwaita application shell with Blueprint resources and a GNOME Calendar-like main window layout containing:

- sidebar placeholder
- header bar
- view stack
- placeholder Month, Week, and Agenda pages
- previous/today/next controls
- view switcher

Acceptance for that milestone:

```sh
cargo check
cargo run
```
