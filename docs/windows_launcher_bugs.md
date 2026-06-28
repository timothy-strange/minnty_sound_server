# Windows Launcher Bugs

Observed during Windows VM testing:

1. Show UI can open extra browser tabs.
   Selecting `Show UI` from the tray context menu opened three tabs: one live UI tab and two deactivated tabs from previous browser sessions.

2. Multiple launcher instances can be started.
   Launching Minnty Sound Server while it is already running can create additional launcher/tray instances. Each instance may try to spawn a server child and can respond to tray/menu events.

Likely causes:

- `open::that("http://127.0.0.1:3000/")` delegates URL handling to the default browser. Edge/Chrome may restore previous discarded or sleeping tabs when opening the UI URL.
- The app currently has no single-instance guard. Every launcher-mode process creates a tray icon and attempts to manage a server child.
- Extra launcher instances may each call `open::that`, making duplicate tab behavior more likely.

Fix plan:

1. Add a single-instance guard for launcher mode.
   If another launcher is already running, the new process should open `http://127.0.0.1:3000/` once and exit without creating another tray icon or spawning another server child.

2. Prefer a simple cross-platform guard.
   Options include binding a local TCP guard port, an OS mutex on Windows, or a lockfile. Choose the smallest reliable implementation.

3. Keep server mode strict.
   If HTTP/UDP ports are already bound, server mode should fail and exit rather than leaving confusing background processes.

4. Consider debouncing Show UI.
   The active launcher can ignore repeated Show UI requests for a short interval to avoid rapid duplicate browser opens.

5. Retest on Windows VM.
   Verify repeated launches do not create extra tray icons/processes and tray `Show UI` opens only one active UI tab.
