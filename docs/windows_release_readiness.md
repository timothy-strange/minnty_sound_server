# Windows Release Readiness Notes

Windows release-readiness items, excluding paid code signing:

1. Clean Windows 11 VM test is most important.
   Test installer and portable zip on machine without dev tools: launch, tray behavior, browser open, server child start/stop, quit cleanup, reboot/relaunch.

2. Firewall behavior.
   Verify first-run prompt, private-network allow path, deny/block path, README symptoms. Confirm clients fail clearly when blocked.

3. SmartScreen / Smart App Control docs.
   Since unsigned forever, README/release notes should be explicit: expected warnings, source/build provenance, checksums, local-network behavior.

4. Defender / VirusTotal sanity check.
   Not trust proof, but catches false positives before users do.

5. Release notes.
   Each GitHub release should state Windows status, unsigned binary, installer vs portable zip behavior, firewall expectations, known limitations, and “tested on Windows 11” once true.

6. Artifact provenance without signing.
   Use checksums already; GitHub artifact attestations or signed checksums are free/cheap alternatives worth considering.

7. Runtime logging.
   Release info-log suppression is handled. Remaining warnings are acceptable; if Windows GUI subsystem hides console anyway, fine.

8. Windows runtime prerequisite.
   The installer should include and run the Microsoft Visual C++ Redistributable 2015-2022 x64. The portable zip should document that this redistributable is required if `VCRUNTIME140.dll` is missing.

9. Real hardware audio test.
   Need WASAPI loopback with common cases: speakers, headphones, Bluetooth, default-device changes, silence, app switching, sleep/wake.

10. Tray/server lifecycle.
   Verify no orphan server process after Quit, crash/restart behavior sane, second launch does not spawn duplicate conflicting server.
