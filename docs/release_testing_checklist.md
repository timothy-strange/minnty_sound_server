# Windows Release Testing Checklist

Work top to bottom. Do the local-only sanity checks first, settle the
launcher single-instance behavior, snapshot, then move to bridged-adapter
network/audio testing. Check off each item as it passes.

Environment: VirtualBox Windows 11 VM, no dev tools installed.

---

## Phase 1 — Local sanity (NAT adapter)

### Launcher / lifecycle
- [ ] Launch from Start Menu shortcut: tray icon appears, browser opens http://127.0.0.1:3000
- [ ] Launch from Desktop shortcut (if created): same behavior
- [ ] Launch from installed path directly: same behavior
- [ ] Tray `Show UI` opens exactly ONE active tab (no restored/duplicate tabs) — verifies fix 46e08c9
- [ ] Tray `Quit`: minnty_sound_server.exe disappears from Task Manager (no orphan)
- [ ] Relaunch after Quit: UI loads, no duplicate/conflicting server
- [ ] Repeated launch while already running does NOT create extra tray icons/processes — single-instance guard (launcher bug #2)
- [ ] Second launch does not spawn a duplicate conflicting server child
- [ ] Reboot VM: app does not auto-start unless expected; manual launch behaves the same

### UI behavior
- [ ] Open UI, switch theme
- [ ] View devices list
- [ ] Change frame duration
- [ ] Start/stop calibration stream
- [ ] No obvious browser console errors

### Audio smoke test (VM audio, treat as rough only)
- [ ] Play audio in Edge / Media Player, confirm meters show activity

### Install / uninstall
- [ ] Installer "Ready to Install" page shows VC++ Redistributable disclosure line — verifies commit 0a446fe
- [ ] VC++ redist install step runs (StatusMsg visible); install completes without error
- [ ] Uninstall via Windows Apps/Programs: app folder and shortcuts removed
- [ ] Portable zip still behaves as documented (VC++ redist already present after installer run)
- [ ] Record actual Edge/SmartScreen wording for zip extraction/run (for README/release notes)

### Snapshot
- [ ] Take VM snapshot named `installed-local-ok` before changing networking

---

## Phase 2 — Network + audio (Bridged adapter)

- [ ] Switch VM to Bridged Adapter, confirm VM has a LAN IP
- [ ] First-run Windows Firewall prompt appears; test private-network allow path
- [ ] Client on another LAN machine can reach the server and stream audio
- [ ] Test firewall deny/block path: clients fail clearly (matches README symptoms)
- [ ] Real client audio streaming plays correctly; meters track playback

---

## Phase 3 — Real-hardware audio (WASAPI loopback, non-VM)

- [ ] Speakers
- [ ] Headphones
- [ ] Bluetooth output
- [ ] Default-device change while streaming
- [ ] Silence handling
- [ ] App switching during playback
- [ ] Sleep / wake

---

## Phase 4 — Release provenance & docs

- [ ] Defender / VirusTotal sanity check on installer + portable zip (catch false positives)
- [ ] Checksums published for artifacts; consider GitHub artifact attestations
- [ ] Release notes state: Windows status, unsigned binary, installer vs zip behavior, firewall expectations, known limitations
- [ ] Update README/notes to "tested on Windows 11" once Phases 1–2 pass
- [ ] Confirm release-build info-log suppression; remaining warnings acceptable

---

## Notes / observations

(Record SmartScreen wording, unexpected behavior, and anything to file as a bug here.)
