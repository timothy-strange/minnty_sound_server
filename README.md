# Minnty Sound Server

Minnty Sound Server streams audio from a PC to Minnty client devices on the same local network. It captures the audio that is playing on the server computer, encodes it with Opus, and sends it over UDP to registered clients.

The server includes a tray launcher and a browser-based control UI at `http://127.0.0.1:3000`.

## Status

Minnty Sound Server `1.0.0` is the first stable release for normal use on supported Windows and Linux systems.

## Windows Runtime Requirement

The Windows installer includes the Microsoft Visual C++ Redistributable 2015-2022 x64 and installs it automatically if needed.

If you use the portable Windows zip instead, your system must already have that redistributable installed. If Windows reports that `VCRUNTIME140.dll` was not found, install the official Microsoft redistributable:

https://aka.ms/vs/17/release/vc_redist.x64.exe

## Supported Platforms

- Linux: PulseAudio capture, GTK tray launcher, browser UI, LAN discovery, Opus streaming, media controls, and server volume control.
- Windows: WASAPI loopback capture, tray launcher, browser UI, LAN discovery, Opus streaming, media controls, and server volume control.

## Downloads

Release builds are published on the GitHub Releases page.

Expected release artifacts:

- Linux AppImage: `minnty-sound-server-vX.Y.Z-linux-x86_64.AppImage`
- Linux DEB: `minnty-sound-server-vX.Y.Z-linux-x86_64.deb`
- Linux RPM: `minnty_sound_server-X.Y.Z-*.rpm`
- Linux tarball: `minnty-sound-server-vX.Y.Z-linux-x86_64.tar.gz`
- Windows installer zip: `minnty-sound-server-vX.Y.Z-windows-x86_64-setup.zip`
- Windows portable zip: `minnty-sound-server-vX.Y.Z-windows-x86_64-portable.zip`
- Checksums: `SHA256SUMS.txt`

## Linux Installation

### AppImage

Download the AppImage, make it executable, then run it:

```bash
chmod +x minnty-sound-server-vX.Y.Z-linux-x86_64.AppImage
./minnty-sound-server-vX.Y.Z-linux-x86_64.AppImage
```

### RPM

Install the RPM using your distribution's package manager. For example:

```bash
sudo dnf install ./minnty_sound_server-X.Y.Z-*.rpm
```

### DEB

Install the DEB using your distribution's package manager. For example:

```bash
sudo apt install ./minnty-sound-server-vX.Y.Z-linux-x86_64.deb
```

The DEB installs:

- executable: `/usr/bin/minnty_sound_server`
- desktop entry: `/usr/share/applications/minnty-sound-server.desktop`
- icon: `/usr/share/icons/hicolor/scalable/apps/minnty-sound-server.svg`

The RPM installs the same executable, desktop entry, and icon paths.

### Tarball

Extract the tarball and run the binary. The tarball does not install a desktop entry or icon.

```bash
tar -xzf minnty-sound-server-vX.Y.Z-linux-x86_64.tar.gz
./minnty-sound-server-vX.Y.Z-linux-x86_64/minnty_sound_server
```

Linux builds require compatible system GTK, PulseAudio, and D-Bus libraries at runtime.

## Windows Installation

Recommended: download the Windows installer zip, extract it, then run the setup executable. It installs Minnty Sound Server and the Microsoft Visual C++ Redistributable required by the Windows build if needed.

Portable option: download the Windows portable zip, extract it, then run `minnty_sound_server.exe`. The portable zip requires the Microsoft Visual C++ Redistributable 2015-2022 x64 to already be installed: https://aka.ms/vs/17/release/vc_redist.x64.exe

The Windows binary is currently unsigned. Windows may show warnings because the executable is new, unsigned, and opens local network sockets.

### Windows Firewall

On first run, Windows Firewall may ask whether to allow Minnty Sound Server to communicate on private and public networks.

Allow network access for networks where you want to use Minnty. For typical home use, allow private networks. If your home network is currently classified by Windows as public, you may need to allow public networks too or change the network profile to private in Windows settings.

If network access is blocked, clients may not discover the server or receive audio.

### Windows SmartScreen

Microsoft Defender SmartScreen may warn that the app is unrecognized. This is expected for an unsigned early release.

If you trust the release, you can usually choose `More info` and then `Run anyway`.

### Smart App Control

On some Windows 11 systems, Smart App Control may block unsigned apps more aggressively and may not offer a normal override. If Smart App Control prevents Minnty Sound Server from running, you may need to disable Smart App Control in Windows Security or build the server from source.

Only disable security features if you understand the tradeoff and trust the software you are running.

## Using Minnty Sound Server

1. Run the server application.
2. The tray launcher starts the server and opens the browser UI.
3. Play audio on the PC.
4. In the UI, select the active audio output.
5. Click `Start Stream`.
6. Connect from a Minnty client on the same local network.

The UI also includes a calibration stream that generates a simple test beat without requiring music or system audio.

## Network Behavior

Minnty Sound Server is designed for local-network use.

- Web UI: `http://127.0.0.1:3000`
- UDP audio/control port: `40110`
- Discovery: mDNS service `_minnty._udp.local.`

Clients must be on the same LAN, VPN, or tethered network path that allows UDP traffic and mDNS discovery.

## Privacy

Minnty Sound Server does not collect telemetry, analytics, crash reports, account data, personal information, or usage statistics.

The app does not transmit information to Minnty servers or any external internet service. Its network activity is limited to the user's own local network: LAN discovery, local control messages, and audio packets sent to clients that register with the server. The browser UI is served from the local machine at `127.0.0.1`.

Audio is streamed only to registered clients on the local network while streaming is active.

## Building From Source

Install Rust stable and platform build dependencies.

Linux requirements include:

- Rust stable
- CMake
- C/C++ build tools
- GTK 3 development libraries
- PulseAudio development libraries
- D-Bus development libraries

Build and test:

```bash
cargo build
cargo test
```

Release build:

```bash
cargo build --release
```

Optional network impairment test UI:

```bash
cargo build --features net_impairment_ui
```

## Release Builds

Release artifacts are built by GitHub Actions from git tags matching `v*`.

The release workflow builds:

- Linux tarball
- Linux RPM
- Linux DEB
- Linux AppImage
- Windows installer zip
- Windows portable zip containing `minnty_sound_server.exe`
- SHA256 checksums

To create a release from a prepared commit:

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

GitHub Actions then builds from that exact tag and publishes artifacts to GitHub Releases.

## License

Licensed under the Apache License, Version 2.0. See `LICENSE`.
