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

Status: Complete

Tasks:

- [x] Define `Calendar` model:
  - [x] id
  - [x] name
  - [x] color
  - [x] visible
  - [x] read-only
  - [x] source/backend kind
- [x] Define `Event` model:
  - [x] id/uid
  - [x] calendar id
  - [x] title
  - [x] start/end
  - [x] all-day
  - [x] location
  - [x] description
  - [x] recurrence placeholder
  - [x] reminders placeholder
- [x] Define repository traits.
- [x] Implement an in-memory repository.
- [x] Add start-inclusive/end-exclusive timed event range queries.

Verification:

```text
cargo test
cargo check
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

Note: the repository currently exposes timed range queries; the pure month projection handles all-day and timed events for view rendering.

### Phase 5: Month view MVP (continuous week scrolling + no auto-selection)

Goal: first useful event-rendering view with GNOME Calendar-like continuous
vertical week scrolling.

Status: Complete

Architecture:

- **15-week-row buffer:** 5 visible + 5 buffered above + 5 buffered below,
  managed by `calendar::weeks_buffer::WeeksBuffer`.
- **GtkScrolledWindow** with an automatic overlay scrollbar. Using a
  non-`never` vertical policy keeps tall buffered content from propagating its
  minimum height into the window. Native smooth/kinetic scrolling (touchpad)
  passes through untouched. A single discrete-only
  `EventControllerScroll` at `CAPTURE` phase intercepts mouse-wheel events
  and snaps the adjustment by exactly one row per notch, returning `STOP`.
- **Row recycling:** when the scroll adjustment value approaches the top or
  bottom of the 15-row content (within 0.5&times;row_height of an edge), the
  `WeeksBuffer` is shifted by &pm;1 week and the adjustment value is
  counter-adjusted to preserve the apparent visual position. After each shift
  the 105 cells are immediately repopulated from cached calendar/event data.
- **No automatic day selection:** `selected_date` is `None` on initial load
  and after Previous/Next/Today navigation. Scrolling does not change
  selection. The first click on a day selects it; a second click on an
  already-selected empty day fires the quick-add placeholder.
- **Viewport-centre title and `other-month` styling:** the title month and
  dimmed-month styling are derived from the Thursday of whichever buffer row
  falls at the viewport's vertical centre pixel
  (`(value + page_size/2) / row_height`). The month-changed callback fires
  on every adjustment tick when the centre pixel's month changes, so the
  header updates smoothly during scrolling — not only at buffer-recycle
  boundaries.
- **Calendar-month navigation:** Previous and Next target the centre
  month's previous/next calendar month, then set the first visible row's
  Monday to the Monday on/before that month's 1st. Today starts from the
  Monday on/before the current month's 1st and shifts the buffer just
  enough to make today's week visible (handles six-week months). Neither
  auto-selects a day.
- **Stable content height (no feedback loop):** the content's `height-request`
  and row height are computed from the initial viewport size and remain fixed.
  Resizing may expose more or fewer complete week rows, but scroll calculations
  remain tied to the rows' actual size and cannot feed child height back into
  the viewport's page size.
- **No day-button focus:** day buttons are constructed with `can_focus:
  false` so the initial GTK keyboard-focus highlight cannot be mistaken for
  a selection. Mouse clicks still work for selection and quick-add.
- **Event projection:** `project_month` is called for every unique
  (year, month) pair among the 105 dates in the 15-row buffer; results are
  indexed by date into a `HashMap<NaiveDate, DayProjection>` and looked up
  per cell during rendering.

Tasks:

- [x] Generate month grid dates (via `calendar_grid::month_grid`).
- [x] Highlight today and selected active date.
- [x] Render events as colored chips.
- [x] Filter by visible calendars.
- [x] Connect date navigation (Previous, Today, Next button actions).
- [x] Open quick-add placeholder from the new-event action or by re-clicking a selected empty day.
- [x] Month/year title updates on navigation.
- [x] In-memory repository seeded with test calendar + events.
- [x] Preserve Week/Agenda placeholders, sidebar, responsive layout, About/Quit.
- [x] Continuous week scrolling via 15-row `WeeksBuffer` + `GtkScrolledWindow`.
- [x] Row recycling at scroll edges.
- [x] No automatic day selection (empty on init, navigation, Today).
- [x] Dominant-month title updated during scrolling.
- [x] Event chips refresh across all 105 cells after buffer shift.
- [x] Re-use `project_month` by projecting all months covered by the buffer
      and indexing results by date.

Verification:

```text
cargo test
cargo run
```

Manual check: month grid works, events display, navigation changes the
visible month, vertical scrolling moves through weeks continuously, no day
is auto-selected, today styling stays distinct from selection.

Manual runtime verification passed: continuous scrolling works, the bottom
view switcher remains visible, and no allocation warnings are emitted.

**Phase 5 visual refinements (post-baseline, not altering accepted status):**

- Today pill: `.monthview-day-label` uses accent-background/foreground,
  `font-weight: 700`, `border-radius: 6px`, `padding: 1px 6px`. Today wins
  over `other-month` and `first-day` styling. The selected cell retains a
  light accent background behind the today pill.
- Initial‑week position: `WeeksBuffer` starts at `monday_of_week(today)`
  on both initial load and Today action. Selection stays `None`.
- Month boundaries: day 1 label displays the full month name and gets a
  `first-day` CSS class with grey pill styling (light/dark variant). Cells
  for days 1–7 receive `separator-top`; day 1 additionally receives
  `separator-side`. Separator classes are recomputed in both `repopulate_rows`
  and `refresh_cell_styles`.
- Fix attempt 1/3 (post-user verification of unstaged refinement) —
  **REJECTED** in diff review:
  - CSS: Added `button.monthview-cell.other-month .monthview-day-label.first-day`
    rules to preserve first-day white-on-dark-2 (black-on-light-3 in dark mode)
    even when the cell is `other-month`. Today rules already come last.
  - Scroll: `setup_scroll` now defers the initial adjustment positioning until
    the adjustment range (`upper - page_size`) can accommodate
    `VISIBLE_START × row_height`. Uses a one-shot idle retry; `initialized`
    stays `false` so recycle callbacks cannot shift the buffer while waiting.
    After successful positioning, fires month-changed callback and refreshes
    cell styles to align with the true centre month.
  - Sidebar: OverlaySplitView now has `min-sidebar-width: 300`,
    `max-sidebar-width: 340`, `sidebar-width-fraction: 0.33` to guarantee
    →288 sp for the date-chooser AdwBin.
  - Rejection reasons:
    1. `setup_scroll` lines 780–788 recursively enqueue another idle callback
       whenever the range is not ready (unbounded loop). Same issue for the
       zero-height branch around 736–745. Must be replaced with finite/event-
       driven lifecycle.
    2. CSS today precedence is not final for all combinations: `today.other-month`
       and `other-month.day-label.first-day` tie at equal specificity (0‑4‑2),
       and `first-day` was placed later, so first-day colours override the accent
       for `today + other-month + first-day`. Today must win for ordinary,
       other-month, first-day, and selected combinations.

- Fix attempt 2/3 — replaced idle retry with two notify handlers on the
  adjustment (`page-size`, `upper`) installed in `constructed()` — failed
  **3/3** in acceptance testing (escalated by worker).
  - Runtime diagnostics (view the user's real display at app startup):
    - scroll height 526, intended row 105.2, requested box 1578, target 526.
    - First three setup passes: box allocation 885 despite min/natural 1578;
      adjustment upper 885, page 526, range 359 → target cannot be set.
    - Later pass: box allocation/upper finally 1578, range 1052.
  - **Root cause:** live allocation race.  `set_height_request` does not
    propagate synchronously to the adjustment range; the notify handlers
    catch the update one or more layout cycles later, but during those
    cycles `initialized` remains `false` and the viewport opens at value 0
    (nine content rows visible, five pre-buffer weeks visible).
  - **Expert-directed correction:**
    1. Removed `eprintln!` diagnostics and the obsolete
       `notify::upper` / `notify::page-size` handlers.
    2. `notify::height` and then `map` both failed to invoke setup in the live
       widget hierarchy; the latter was confirmed by temporary diagnostics
       producing no output. A `size_allocate` override also left nine natural-
       height rows because it changed requests during the active allocation
       pass and initialized before the resize took effect. Setup now runs from
       the previously confirmed `realize` + one bounded idle callback, outside
       allocation; explicit geometry means no retry or polling is needed.
    3. Stores 15 row-box references (`row_boxes`) for explicit per-row
       sizing instead of relying on the outer homogeneous box's
       allocation to catch up.
    4. On first valid allocation, computes a whole-pixel row height from
       `viewport / 5`,
       sets each row's `height_request` to `row_height`, sets the outer
       content-box `height_request` to `15 * row_height`, and then
       **explicitly establishes every adjustment parameter** atomically:
       `lower=0`, `upper=15*row_h`, `page_size=viewport`, `step=row_h`,
       `page=viewport`, `value=VISIBLE_START*row_h`.
    5. All under the `recycling_guard`; marks `initialized` true
       immediately after.
    6. If GTK later recomputes the adjustment from the now-explicit
       content geometry, it converges to the same bounds/value rather
       than resetting to zero.
    7. Preserves go_today/month navigation, 5-row viewport, 15-row
       buffer, event chips, CSS fixes, and sidebar constraints.
    8. The responsive split-view breakpoint now matches GNOME Calendar at
       1000sp instead of 700sp, preventing startup's 360px minimum allocation
        from dividing the content to 241px before the default size settles.
  - Awaiting manual visual verification (`cargo run`).

**Unstaged CSS-architecture refinement (awaiting manual verification):**

   - **Topbar/title reference change** (preserved from first pass): the topbar
     dominant month and all month-changed callbacks now derive from the first
     visible complete week (row `VISIBLE_START`, Thursday col 3) instead of the
     viewport centre.  Cell dimming (`other-month`) continues to use the
     viewport centre via `ref_year_month()` → `viewport_center_ym()`.  Fixes
     the initial July 20, 2026 viewport showing "August" when the first visible
     week is still July.
   - **GNOME-style `calendar-view` root:** Month template CSS node changed from
     `monthview` to `calendar-view` (shared root) with style class `month-view`.
     All Month selectors updated from `monthview …` to
     `calendar-view.month-view …`.  The root sets `font-size: 10pt`; Month day
     labels inherit this baseline (no forced 11pt).  Weekday labels match GNOME
     with uppercase abbreviations, the `heading` class, dim colour, and 12px inset.
   - **Relative event sizing:** Chip containers use `font-size: 0.9rem` (GNOME
     events.css pattern) — no explicit `min-height`; natural layout prevents
     clipping.  Overflow labels use CSS `font-size: smaller`.
   - **No artificial shared-class API** per leaf element; inheritance from
     `calendar-view` and existing widget classes are faithful to GNOME.
   - **Future Week/Agenda notes** (no speculative selectors added):
     Week view should use Libadwaita `heading`/`title-2` for day numbers,
     10pt for hour labels, `heading dimmed` for weekday names.  Agenda should
     use `caption-heading` for day headings.  Shared event widgets across views
     use 0.9 rem.
   - Sidebar/mini-calendar typography unchanged; Week/Agenda placeholder
     `title-1` empty-state headings preserved.
   - **Fix attempt 1 (REJECTED):** initial `first_visible_week_ym()` always
     read row `VISIBLE_START` from the buffer.  This was correct only at the
     centred initial position; during ordinary scrolling within the 15-row
     buffer the title would remain stuck and month-changed callbacks would not
     fire because the visible rows shift without recycling.  Fixed by deriving
     the first completely visible row from the live scroll adjustment and row
     height, using `ceil(val / row_h)` so a partially clipped top row during
     smooth/kinetic scrolling falls through to the next complete row.
     Pre‑initialisation fallback stays at `VISIBLE_START`.
   - No staged changes; run acceptance commands below on the working tree.

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
