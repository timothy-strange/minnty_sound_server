# Windows Release Readiness Notes

These notes summarize the current Windows release-readiness assessment for the public `v0.9.0` release and suggested follow-up before a final `1.0.0` release.

## Already Addressed

- The repository is public, so users can inspect the source.
- Release binaries are published through GitHub Releases.
- Release artifacts are built from repository source by GitHub Actions, rather than from an opaque local machine build.
- Release checksums are published.
- The README explains that the Windows binary is currently unsigned.
- The README documents expected Windows Firewall, SmartScreen, and Smart App Control behavior.
- The README includes a privacy section stating that the server does not collect telemetry, analytics, crash reports, account data, personal information, or usage statistics.
- The README explains that network activity is local-network focused: local UI, mDNS discovery, UDP control, and UDP audio sent to registered clients.

For a public pre-`1.0` testing release, this is a reasonable baseline.

## Recommended Before Windows 1.0

- Code-sign the Windows executable if the project is intended for broad non-developer use. This is the main remaining Windows distribution gap. Without signing, SmartScreen and Smart App Control friction will remain.
- Test the release zip on a clean Windows 11 machine or VM, not only on a development machine.
- Confirm first-run Windows Firewall behavior on a clean system. Test both the allowed-private-network path and the blocked path, and make sure the README accurately describes the symptoms.
- Run a basic Defender/VirusTotal sanity check for release artifacts. This is not a trust guarantee, but it can catch false positives early.
- Consider GitHub artifact attestations or signed checksums for stronger release provenance.
- Add human-readable release notes to each GitHub release. The release page should summarize the status, supported platforms, known limitations, and Windows-specific warnings.

## Logging Follow-Up

The local source still contains `now playing changed` info logs in `src/transport/udp.rs`. They are not debug-only; the logging macros currently print unconditionally.

These logs may not be visible when launching the Windows release normally because the release build uses the Windows GUI subsystem, but they still exist and can appear when run from a terminal or captured by a parent process.

Suggested cleanup before `1.0`:

- Keep broadcasting now-playing updates to clients while playback is active.
- Stop logging position-only metadata updates.
- Log only meaningful metadata changes, such as artist, title, status, duration, or track identity changes.

This is not a blocker for `v0.9.0`, but it is worth cleaning before a polished release.
