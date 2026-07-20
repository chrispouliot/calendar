# Calendar App Plan

## Goal

Build a Rust/GTK4/Libadwaita calendar application that closely mirrors the GNOME Calendar UI and user experience, but uses this app's own backend for CalDAV sync, local persistence, and reminders instead of Evolution Data Server/EDS.

GNOME Calendar was cloned for reference to:

```text
/tmp/gnome-calendar
```

This repository is currently a minimal Rust scaffold:

```text
Cargo.toml      # package calendar, Rust 2024
src/main.rs     # hello world only
flake.nix       # GTK4/libadwaita dev shell, includes blueprint-compiler
```

Current Rust dependencies:

```toml
adw = { version = "0.9.2", package = "libadwaita" }
gtk = { version = "0.11.4", package = "gtk4" }
```

The Nix shell already provides important GNOME/Rust tooling:

- Rust toolchain
- `pkg-config`
- GTK4
- Libadwaita
- `blueprint-compiler`
- Meson/Ninja, though the initial Rust app can use `build.rs` instead
- GSettings/icon/theme runtime data

## GNOME Calendar reference summary

The GNOME Calendar repository used for research is:

```text
https://gitlab.gnome.org/GNOME/gnome-calendar.git
```

Local clone:

```text
/tmp/gnome-calendar
```

Important reference areas:

```text
/tmp/gnome-calendar/src/gui/
/tmp/gnome-calendar/src/gui/views/
/tmp/gnome-calendar/src/gui/common/
/tmp/gnome-calendar/src/gui/event-editor/
/tmp/gnome-calendar/src/gui/calendar-management/
/tmp/gnome-calendar/src/gui/importer/
/tmp/gnome-calendar/src/core/
/tmp/gnome-calendar/src/search/
/tmp/gnome-calendar/src/utils/
```

## How GNOME Calendar uses Blueprint UI files

GNOME Calendar stores Blueprint files under `src/gui` and subdirectories. It has about 46 `.blp` files.

Main locations:

```text
/tmp/gnome-calendar/src/gui/*.blp
/tmp/gnome-calendar/src/gui/views/*.blp
/tmp/gnome-calendar/src/gui/common/*.blp
/tmp/gnome-calendar/src/gui/event-editor/*.blp
/tmp/gnome-calendar/src/gui/calendar-management/*.blp
/tmp/gnome-calendar/src/gui/importer/*.blp
```

Build pattern used by GNOME Calendar:

1. Compile `.blp` to `.ui` using `blueprint-compiler batch-compile`.
2. List generated `.ui` files in `.gresource.xml` manifests.
3. Compile resources into the binary with `gnome.compile_resources()`.
4. Load templates at runtime from resource paths.

Representative Meson pattern:

```meson
blueprints = custom_target(
  'blueprints',
  input: files('widget.blp'),
  output: '.',
  command: [
    find_program('blueprint-compiler'),
    'batch-compile',
    '@OUTPUT@',
    '@CURRENT_SOURCE_DIR@',
    '@INPUT@',
  ],
)

built_sources += gnome.compile_resources(
  'resource-name',
  'resource.gresource.xml',
  dependencies: blueprints,
)
```

Resource prefixes in GNOME Calendar include:

```text
/org/gnome/calendar/ui/gui
/org/gnome/calendar/ui/views
/org/gnome/calendar/ui/common
/org/gnome/calendar/ui/event-editor
/org/gnome/calendar/ui/gui/calendar-management
/org/gnome/calendar/ui/gui/importer
```

For this Rust app, use the same conceptual pipeline, likely via `build.rs`:

1. Run `blueprint-compiler` over `data/ui/**/*.blp`.
2. Generate `.ui` files into an output/build directory.
3. Compile a `.gresource.xml` with `glib-compile-resources`.
4. Register resources in `main.rs` before constructing templates.
5. Use gtk-rs `CompositeTemplate` widgets with resource paths.

Suggested resource path namespace for this app:

```text
/dev/chris/calendar/ui/window.ui
/dev/chris/calendar/ui/views/month-view.ui
/dev/chris/calendar/ui/views/week-view.ui
/dev/chris/calendar/ui/views/agenda-view.ui
```

Adjust the namespace once the application ID is decided.

## GNOME Calendar UI architecture

The application itself is an `AdwApplication` subclass and creates the main window imperatively on activation.

Reference files:

```text
/tmp/gnome-calendar/src/main.c
/tmp/gnome-calendar/src/gui/gcal-application.c
```

The main window is defined in Blueprint:

```text
/tmp/gnome-calendar/src/gui/gcal-window.blp
/tmp/gnome-calendar/src/gui/gcal-window.c
```

High-level window structure:

```text
Adw.ApplicationWindow
  Adw.ToastOverlay
    GcalDropOverlay
      Adw.OverlaySplitView
        sidebar:
          GcalDateChooser
          GcalCalendarList
        content:
          Adw.ToolbarView
            Adw.HeaderBar
            Adw.ViewStack
              GcalMonthView
              GcalWeekView
              GcalAgendaView
    bottom ActionBar / mobile navigation controls
  GcalQuickAddPopover
  GcalEventEditorDialog
  GcalCalendarManagementDialog
```

Main view files:

```text
/tmp/gnome-calendar/src/gui/views/gcal-view.c
/tmp/gnome-calendar/src/gui/views/gcal-view.h
/tmp/gnome-calendar/src/gui/views/gcal-month-view.blp
/tmp/gnome-calendar/src/gui/views/gcal-month-view.c
/tmp/gnome-calendar/src/gui/views/gcal-week-view.blp
/tmp/gnome-calendar/src/gui/views/gcal-week-view.c
/tmp/gnome-calendar/src/gui/views/gcal-agenda-view.blp
/tmp/gnome-calendar/src/gui/views/gcal-agenda-view.c
```

The three main views are:

- Month view: month grid, event chips, overflow popovers.
- Week view: seven-day hour grid, all-day/timed blocks, current-time positioning, zoom support.
- Agenda view: vertical list of upcoming events grouped by day.

GNOME Calendar has a `GcalView` interface with operations equivalent to:

- set/get active date
- get next date
- get previous date
- emit create-event
- emit create-event-detailed
- emit event-activated

In Rust, mirror this as a trait/controller boundary rather than copying the C interface directly.

## How GNOME Calendar connects code to templates

In C, each custom widget does the following:

1. Declares a GObject type.
2. Stores child widgets as struct fields.
3. Calls `gtk_widget_class_set_template_from_resource()` in class init.
4. Binds template children with `gtk_widget_class_bind_template_child()`.
5. Binds callbacks with `gtk_widget_class_bind_template_callback()`.
6. Calls `gtk_widget_init_template()` in instance init.

Rust/gtk-rs equivalent:

- Use `glib::ObjectSubclass`.
- Use `gtk::CompositeTemplate`.
- Add `#[template(resource = "...")]`.
- Add `#[template_child]` fields.
- Register callbacks/actions in the subclass implementation or object construction.

Useful GNOME Calendar files for template binding examples:

```text
/tmp/gnome-calendar/src/gui/gcal-window.c
/tmp/gnome-calendar/src/gui/views/gcal-month-view.c
/tmp/gnome-calendar/src/gui/views/gcal-week-view.c
/tmp/gnome-calendar/src/gui/views/gcal-agenda-view.c
```

## GNOME Calendar user behavior to mirror

### Navigation

GNOME Calendar supports:

- Month, week, and agenda/list views.
- A shared active date propagated to all views.
- Previous/next navigation depending on active view:
  - month view changes month
  - week view changes week
  - agenda/list changes by date range/day behavior
- Today button jumps to current date.
- Sidebar mini date chooser selects active date.
- View switching by header controls and keyboard shortcuts.
- Responsive layout using Adwaita breakpoints/split view behavior.

Reference files:

```text
/tmp/gnome-calendar/src/gui/gcal-window.c
/tmp/gnome-calendar/src/gui/gcal-window.blp
/tmp/gnome-calendar/src/gui/common/gcal-date-chooser.c
/tmp/gnome-calendar/src/gui/common/gcal-date-chooser.blp
```

### Event creation

GNOME Calendar has two creation paths:

1. Quick-add popover
   - Triggered from an empty slot/day or New Event action.
   - User enters summary/title.
   - User selects target calendar.
   - Can save directly or open detailed editor.

2. Detailed event editor dialog
   - Used for full create/edit flow.
   - Handles schedule, all-day, recurrence, reminders, notes, attendees display, calendar selection, delete.

Reference files:

```text
/tmp/gnome-calendar/src/gui/gcal-quick-add-popover.blp
/tmp/gnome-calendar/src/gui/gcal-quick-add-popover.c
/tmp/gnome-calendar/src/gui/event-editor/gcal-event-editor-dialog.blp
/tmp/gnome-calendar/src/gui/event-editor/gcal-event-editor-dialog.c
/tmp/gnome-calendar/src/gui/event-editor/gcal-summary-section.c
/tmp/gnome-calendar/src/gui/event-editor/gcal-schedule-section.c
/tmp/gnome-calendar/src/gui/event-editor/gcal-reminders-section.c
/tmp/gnome-calendar/src/gui/event-editor/gcal-notes-section.c
```

### Event viewing/editing/deleting

Clicking an event opens a preview popover with:

- title
- date/time
- location
- description
- meeting links if detected
- edit button

Deleting an event uses an undo toast. The actual deletion is delayed until the toast times out unless the user clicks undo.

Reference files:

```text
/tmp/gnome-calendar/src/gui/gcal-event-popover.blp
/tmp/gnome-calendar/src/gui/gcal-event-popover.c
/tmp/gnome-calendar/src/gui/gcal-event-widget.blp
/tmp/gnome-calendar/src/gui/gcal-event-widget.c
/tmp/gnome-calendar/src/gui/gcal-meeting-row.c
```

### Calendar/source management

GNOME Calendar has a calendar management dialog with:

- calendar list
- visibility toggles
- color display/editing
- edit calendar page
- new calendar page
- remote WebDAV/ICS subscription flow

Reference files:

```text
/tmp/gnome-calendar/src/gui/calendar-management/gcal-calendar-management-dialog.blp
/tmp/gnome-calendar/src/gui/calendar-management/gcal-calendar-management-dialog.c
/tmp/gnome-calendar/src/gui/calendar-management/gcal-calendars-page.c
/tmp/gnome-calendar/src/gui/calendar-management/gcal-edit-calendar-page.c
/tmp/gnome-calendar/src/gui/calendar-management/gcal-new-calendar-page.c
/tmp/gnome-calendar/src/gui/gcal-calendar-list.c
/tmp/gnome-calendar/src/gui/common/gcal-calendar-row.c
/tmp/gnome-calendar/src/gui/common/gcal-calendar-combo-row.c
```

### Search/import/sync/reminders

GNOME Calendar includes:

- Header search button and results popover.
- GNOME Shell search provider.
- ICS import via file argument or drag-and-drop.
- Sync action with spinner/checkmark indicator.
- Reminder data editing, but reminder notification delivery is delegated to Evolution/EDS alarm services.

Reference files:

```text
/tmp/gnome-calendar/src/gui/gcal-search-button.c
/tmp/gnome-calendar/src/search/gcal-search-engine.c
/tmp/gnome-calendar/src/gui/importer/gcal-import-dialog.c
/tmp/gnome-calendar/src/gui/importer/gcal-importer.c
/tmp/gnome-calendar/src/gui/gcal-sync-indicator.c
/tmp/gnome-calendar/src/gui/event-editor/gcal-reminders-section.c
```

For this app, reminders must be implemented by our own service.

### Keyboard shortcuts and actions

GNOME Calendar shortcuts include:

```text
F10                 open main menu
F9                  toggle sidebar
Ctrl+N              new event
F8 / Ctrl+Alt+M     manage calendars
F5 / Ctrl+R         synchronize calendars
Ctrl+Q / Ctrl+W     close/quit
Ctrl+F              search
F1                  help
Ctrl+?              shortcuts window
Alt+Left/Page_Up    previous date
Alt+Right/Page_Down next date
Alt+Down/Ctrl+T/Home today
Ctrl+Page_Down      next view
Ctrl+Page_Up        previous view
Alt+1               month view
Alt+2               week view
```

Reference file:

```text
/tmp/gnome-calendar/src/gui/shortcuts-dialog.blp
```

## EDS/Evolution dependencies to replace

GNOME Calendar's backend is built around Evolution Data Server. Do not port that backend directly.

Important EDS-backed reference files:

```text
/tmp/gnome-calendar/src/core/gcal-manager.c
/tmp/gnome-calendar/src/core/gcal-calendar.c
/tmp/gnome-calendar/src/core/gcal-calendar-monitor.c
/tmp/gnome-calendar/src/core/gcal-event.c
/tmp/gnome-calendar/src/core/gcal-timeline.c
/tmp/gnome-calendar/src/core/gcal-range.c
/tmp/gnome-calendar/src/core/gcal-range-tree.c
/tmp/gnome-calendar/src/core/gcal-recurrence.c
```

EDS concepts to replace:

```text
ESourceRegistry       -> our calendar account/source registry
ESource               -> our CalendarSource/Calendar metadata
ECalClient            -> our event repository and CalDAV client
ECalComponent         -> our Event model and iCalendar mapping
ECalClientView        -> our sync/change notification mechanism
ECalComponentAlarm    -> our Reminder model/service
ECredentialsPrompter  -> our account/auth UI
Evolution alarm daemon -> our reminder scheduler/notification service
```

Recommended backend boundaries for this app:

```text
CalendarRepository
EventRepository
Timeline/EventQueryService
SyncService
CalDavClient
ReminderService
NotificationService
```

## Proposed project structure

Suggested Rust layout:

```text
src/
  main.rs
  application.rs
  window.rs

  ui/
    mod.rs
    month_view.rs
    week_view.rs
    agenda_view.rs
    date_chooser.rs
    calendar_list.rs
    event_widget.rs
    event_popover.rs
    quick_add_popover.rs
    event_editor_dialog.rs
    calendar_management_dialog.rs

  model/
    mod.rs
    calendar.rs
    event.rs
    recurrence.rs
    reminder.rs
    date_range.rs

  backend/
    mod.rs
    repository.rs
    memory.rs
    sqlite.rs
    caldav.rs
    sync.rs
    reminders.rs

  timeline/
    mod.rs
    query.rs
    event_index.rs
```

Suggested data/resource layout:

```text
data/
  resources.gresource.xml
  ui/
    window.blp
    views/
      month-view.blp
      week-view.blp
      agenda-view.blp
    common/
      date-chooser.blp
      calendar-list.blp
      calendar-row.blp
    event-editor/
      event-editor-dialog.blp
      schedule-section.blp
      reminders-section.blp
      notes-section.blp
    calendar-management/
      calendar-management-dialog.blp
```

## Phased implementation plan

### Phase 1: GTK/Libadwaita shell

Goal: launch a real Adwaita app window.

Status: In progress

Tasks:

- [x] Replace `src/main.rs` hello world with an `adw::Application`.
- [x] Choose an application ID.
- [x] Add startup/activate handling.
- [x] Add initial actions:
  - [x] quit
  - [x] about
  - [ ] new-event
  - [ ] today
  - [ ] next-date
  - [ ] previous-date
  - [ ] change-view
- [x] Add a placeholder `ApplicationWindow`.

Verification:

```text
cargo check
cargo run
```

Expected result: a blank Libadwaita window launches.

### Phase 2: Blueprint/resource pipeline

Goal: use Blueprint templates and embedded GResources like GNOME Calendar.

Status: Complete

Tasks:

- [x] Add `build.rs`.
- [x] Add `blueprint-compiler` invocation.
- [x] Add `data/resources.gresource.xml`.
- [x] Add `data/ui/window.blp`.
- [x] Register resources at application startup.
- [x] Convert the window to a `CompositeTemplate` loaded from resource.

Verification:

```text
cargo check      (passes)
cargo fmt --all --check  (passes)
cargo clippy --all-targets --all-features -- -D warnings  (passes)
cargo run        (passes manual runtime verification)
```

Expected result: the app launches from a Blueprint-defined window.

### Phase 3: GNOME Calendar-like main layout

Goal: mirror GNOME Calendar's structural UI.

Status: Complete

Tasks:

- [x] Add main window layout:
  - [x] `Adw.ApplicationWindow` (was baseline; unchanged)
  - [x] `Adw.ToastOverlay`
  - [x] `Adw.OverlaySplitView`
  - [x] sidebar
  - [x] `Adw.ToolbarView`
  - [x] `Adw.HeaderBar`
  - [x] `Adw.ViewStack`
- [x] Add placeholder pages:
  - [x] Month (icon + label placeholder)
  - [x] Week (icon + label placeholder)
  - [x] Agenda (icon + label placeholder)
- [x] Add header controls:
  - [x] previous (button with action `win.previous-date`)
  - [x] today (button with action `win.today`)
  - [x] next (button with action `win.next-date`)
  - [x] view switcher (`Adw.ViewSwitcher` bound to stack)
  - [x] new event (button with action `win.new-event`)
  - [x] search placeholder (button in header bar end)
  - [x] menu placeholder (`PopoverMenu` with About and Quit)
- [x] Add sidebar placeholders:
  - [x] custom Adwaita-style mini date chooser with month navigation and selection
  - [x] calendar list (heading + "No calendars yet" label)
- [x] Sidebar toggle works via property bind on `split_view.show-sidebar`
- [x] Responsive breakpoint (max-width: 700sp): collapses split view, narrows view switcher
- [x] Window actions registered as no-ops for `win.previous-date`, `win.next-date`, `win.today`, `win.new-event`

Verification:

```text
cargo check            (passes)
cargo fmt --all --check (passes)
cargo clippy --all-targets --all-features -- -D warnings  (passes)
cargo run              (passes manual runtime verification)
```

Expected result: window visually resembles GNOME Calendar's layout with placeholder views.

Note: Actions have no behaviour yet; wiring to real navigation/event-creation will come in later phases.

### Phase 4: Core model and in-memory backend

Goal: establish EDS-free calendar/event domain objects.

Status: Not started

Tasks:

- [ ] Define `Calendar` model:
  - [ ] id
  - [ ] name
  - [ ] color
  - [ ] visible
  - [ ] read-only
  - [ ] source/backend kind
- [ ] Define `Event` model:
  - [ ] id/uid
  - [ ] calendar id
  - [ ] title
  - [ ] start/end
  - [ ] all-day
  - [ ] location
  - [ ] description
  - [ ] recurrence placeholder
  - [ ] reminders placeholder
- [ ] Define repository traits.
- [ ] Implement an in-memory repository.
- [ ] Add range-query behavior for events.

Verification:

```text
cargo test
cargo check
```

### Phase 5: Month view MVP

Goal: first useful event-rendering view.

Status: Not started

Tasks:

- [ ] Generate month grid dates.
- [ ] Highlight today and selected active date.
- [ ] Render events as colored chips.
- [ ] Filter by visible calendars.
- [ ] Connect date navigation.
- [ ] Open quick-add flow from a day/empty area.

Verification:

```text
cargo test
cargo run
```

Manual check: month grid works, events display, navigation changes the visible month.

### Phase 6: Quick add and event preview

Goal: create and inspect events.

Status: Not started

Tasks:

- [ ] Add quick-add popover:
  - [ ] title entry
  - [ ] calendar selector
  - [ ] Add button
  - [ ] Edit Event button placeholder
- [ ] Add event preview popover:
  - [ ] title
  - [ ] date/time
  - [ ] location/description if present
  - [ ] Edit button placeholder
- [ ] Persist created events to in-memory repository.
- [ ] Refresh month view after creation.

Verification:

```text
cargo check
cargo run
```

Manual check: create an event, see it in month view, click it to see preview.

### Phase 7: Local persistence

Goal: events/calendars survive restart.

Status: Not started

Recommended first persistence backend: SQLite.

Reason: easier range querying, local cache, sync metadata, and reminders than raw `.ics` files.

Tasks:

- [ ] Add SQLite dependency, likely `rusqlite` or `sqlx`.
- [ ] Create schema for:
  - [ ] calendars
  - [ ] events
  - [ ] reminders
  - [ ] sync metadata placeholder
- [ ] Implement repository traits with SQLite.
- [ ] Migrate in-memory development data path to SQLite.

Verification:

```text
cargo test
cargo run
```

Manual check: create event, restart app, event remains.

### Phase 8: Week and agenda views

Goal: complete GNOME Calendar's primary view set.

Status: Not started

Tasks:

- [ ] Week view:
  - [ ] seven-day columns
  - [ ] hour rows
  - [ ] all-day section
  - [ ] timed event blocks
- [ ] Agenda view:
  - [ ] events grouped by day
  - [ ] upcoming/range filtering
- [ ] Shared view controller/trait:
  - [ ] active date
  - [ ] next/previous date
  - [ ] event activation
  - [ ] create event request

Verification:

```text
cargo test
cargo run
```

Manual check: switching views preserves active date and event visibility.

### Phase 9: Detailed event editor

Goal: full create/edit/delete for local events.

Status: Not started

Tasks:

- [ ] Add event editor dialog.
- [ ] Support fields:
  - [ ] title
  - [ ] calendar
  - [ ] start/end
  - [ ] all-day
  - [ ] location
  - [ ] description
- [ ] Save/update existing events.
- [ ] Delete with undo toast.
- [ ] Add recurrence/reminders as placeholders or minimal models first.

Verification:

```text
cargo test
cargo run
```

Manual check: create, edit, delete, undo delete.

### Phase 10: Calendar management

Goal: local calendar management independent of EDS.

Status: Not started

Tasks:

- [ ] Sidebar calendar list with visibility toggles.
- [ ] Calendar management dialog.
- [ ] Add/edit/delete local calendars.
- [ ] Calendar colors applied to event chips.
- [ ] Persist visibility/color/name.

Verification:

```text
cargo test
cargo run
```

Manual check: toggling calendar visibility updates all views.

### Phase 11: CalDAV backend

Goal: own network sync backend.

Status: Not started

Tasks:

- [ ] Add account/source model.
- [ ] Add CalDAV discovery/login flow.
- [ ] Discover calendars from server.
- [ ] Sync events.
- [ ] Store remote metadata:
  - [ ] URL
  - [ ] UID
  - [ ] ETag
  - [ ] sync-token if supported
- [ ] Map iCalendar to app event model.
- [ ] Handle local edits and upload.
- [ ] Handle conflict strategy.

Likely crates to evaluate:

```text
reqwest
quick-xml or another XML parser
icalendar / ical
chrono or time
uuid
tokio or GLib async integration approach
```

Verification:

Use integration tests against a local CalDAV server such as Radicale or another test fixture.

### Phase 12: Reminders and notifications

Goal: replace Evolution alarm notification behavior.

Status: Not started

Tasks:

- [ ] Store reminder definitions in the app database.
- [ ] Compute next reminder occurrences.
- [ ] Run a reminder scheduler while app/background service is active.
- [ ] Use desktop notifications through GIO/GLib APIs.
- [ ] Support dismiss/snooze later.
- [ ] Recompute reminders after sync/edit/delete.

Verification:

```text
cargo test
cargo run
```

Manual check: create event with reminder and receive notification.

## Recommended first work unit

Start with the foundation:

```text
Build the GTK/Libadwaita application shell with Blueprint resources and a GNOME Calendar-like main window layout containing sidebar, header bar, view stack, and placeholder Month/Week/Agenda pages.
```

Acceptance for that first unit:

```text
cargo check
cargo run
```

Expected visible result:

- Libadwaita window launches.
- Header bar resembles GNOME Calendar.
- Sidebar exists with placeholder date chooser/calendar list.
- Main area has Month/Week/Agenda placeholder views.
- View switcher changes visible page.
- Previous/today/next controls are present, even if behavior is initially minimal.

## Notes for future sessions

- Do not copy GNOME Calendar's EDS backend. Use its UI and behavior as a reference only.
- Keep UI and backend boundaries separate from the start.
- Build the app incrementally: first shell/layout, then model, then month view, then event creation, then persistence, then CalDAV/reminders.
- Prefer test coverage for pure date/range/query/backend logic.
- Use manual/diff review for visual GTK layout work unless a suitable GTK test seam is introduced.
- GNOME Calendar's C code is useful for behavior and widget organization, but Rust implementation should use gtk-rs idioms.
