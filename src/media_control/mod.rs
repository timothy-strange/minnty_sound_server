use crate::control::messages::{MediaCommand, NowPlayingMetadata};

pub trait MediaController: Send + Sync {
    fn handle(&self, command: MediaCommand, argument: i64);
    fn now_playing(&self) -> Option<NowPlayingMetadata>;
}

pub struct PlatformMediaController;

impl MediaController for PlatformMediaController {
    fn handle(&self, command: MediaCommand, argument: i64) {
        platform::handle(command, argument);
    }

    fn now_playing(&self) -> Option<NowPlayingMetadata> {
        platform::now_playing()
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use crate::control::messages::{MediaCommand, NowPlayingMetadata, PlaybackStatus};
    use dbus::blocking::Connection;
    use dbus::arg::{RefArg, Variant};
    use std::collections::HashMap;
    use std::time::Duration;

    pub fn handle(command: MediaCommand, argument: i64) {
        crate::log_info!("media control linux command={:?} argument={}", command, argument);
        if matches!(command, MediaCommand::VolumeUp | MediaCommand::VolumeDown) {
            return;
        }

        match dispatch_mpris(command, argument) {
            Ok(player) => crate::log_info!(
                "media control linux complete command={:?} player={}",
                command,
                player
            ),
            Err(e) => crate::log_warn!("media control linux failed command={:?}: {}", command, e),
        }
    }

    fn dispatch_mpris(command: MediaCommand, argument: i64) -> Result<String, Box<dyn std::error::Error>> {
        let connection = Connection::new_session()?;
        let player = find_player_name(&connection)?;
        let proxy = connection.with_proxy(
            player.as_str(),
            "/org/mpris/MediaPlayer2",
            Duration::from_secs(2),
        );

        match command {
            MediaCommand::PlayPause => proxy.method_call("org.mpris.MediaPlayer2.Player", "PlayPause", ())?,
            MediaCommand::Play => proxy.method_call("org.mpris.MediaPlayer2.Player", "Play", ())?,
            MediaCommand::Pause => proxy.method_call("org.mpris.MediaPlayer2.Player", "Pause", ())?,
            MediaCommand::Next => proxy.method_call("org.mpris.MediaPlayer2.Player", "Next", ())?,
            MediaCommand::Previous => proxy.method_call("org.mpris.MediaPlayer2.Player", "Previous", ())?,
            MediaCommand::SeekRelativeMs => {
                let offset_microseconds = argument.saturating_mul(1_000);
                proxy.method_call("org.mpris.MediaPlayer2.Player", "Seek", (offset_microseconds,))?
            }
            MediaCommand::VolumeUp | MediaCommand::VolumeDown => return Err("volume commands are not MPRIS commands".into()),
        }
        Ok(player)
    }

    fn find_player_name(connection: &Connection) -> Result<String, Box<dyn std::error::Error>> {
        let dbus = connection.with_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            Duration::from_secs(2),
        );
        let (names,): (Vec<String>,) = dbus.method_call("org.freedesktop.DBus", "ListNames", ())?;
        names
            .into_iter()
            .find(|name| name.starts_with("org.mpris.MediaPlayer2."))
            .ok_or_else(|| "no MPRIS media players found".into())
    }

    pub fn now_playing() -> Option<NowPlayingMetadata> {
        read_now_playing().ok().flatten()
    }

    fn read_now_playing() -> Result<Option<NowPlayingMetadata>, Box<dyn std::error::Error>> {
        let connection = Connection::new_session()?;
        let player = find_player_name(&connection)?;
        let proxy = connection.with_proxy(
            player.as_str(),
            "/org/mpris/MediaPlayer2",
            Duration::from_secs(2),
        );

        // GetAll returns a{sv} directly — no extra Variant wrapper unlike Properties.Get.
        let (all_props,): (HashMap<String, Variant<Box<dyn RefArg>>>,) = proxy.method_call(
            "org.freedesktop.DBus.Properties",
            "GetAll",
            ("org.mpris.MediaPlayer2.Player",),
        )?;

        let playback_status = all_props
            .get("PlaybackStatus")
            .and_then(|v| v.0.as_str())
            .map(|s| match s {
                "Playing" => PlaybackStatus::Playing,
                "Paused" => PlaybackStatus::Paused,
                "Stopped" => PlaybackStatus::Stopped,
                _ => PlaybackStatus::Unknown,
            })
            .unwrap_or(PlaybackStatus::Unknown);

        let (title, artist) = match all_props
            .get("Metadata")
            .and_then(|v| dbus::arg::cast::<HashMap<String, Variant<Box<dyn RefArg>>>>(&*v.0))
        {
            Some(metadata_map) => {
                let title = metadata_map
                    .get("xesam:title")
                    .and_then(|v| v.0.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let artist = metadata_map
                    .get("xesam:artist")
                    .and_then(|v| v.0.as_iter())
                    .and_then(|mut it| it.next().and_then(|v| v.as_str()))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                (title, artist)
            }
            None => {
                crate::log_warn!("media control metadata decode failed player={}", player);
                (String::new(), String::new())
            }
        };

        Ok(Some(NowPlayingMetadata { artist, title, playback_status }))
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use crate::control::messages::MediaCommand;
    use crate::control::messages::NowPlayingMetadata;
    use std::process::Command;

    pub fn handle(command: MediaCommand, _argument: i64) {
        let key = match command {
            MediaCommand::PlayPause | MediaCommand::Play | MediaCommand::Pause => 0xB3u8,
            MediaCommand::Next => 0xB0u8,
            MediaCommand::Previous => 0xB1u8,
            MediaCommand::SeekRelativeMs | MediaCommand::VolumeUp | MediaCommand::VolumeDown => {
                crate::log_warn!("media control windows unsupported command={:?}", command);
                return;
            }
        };
        crate::log_info!("media control windows command={:?} key=0x{:X}", command, key);
        let script = format!(
            "Add-Type -MemberDefinition '[DllImport(\"user32.dll\")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);' -Name Native -Namespace Win32; [Win32.Native]::keybd_event({key},0,0,[UIntPtr]::Zero); [Win32.Native]::keybd_event({key},0,2,[UIntPtr]::Zero)"
        );
        let result = Command::new("powershell")
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(script)
            .status();
        match result {
            Ok(status) if status.success() => {
                crate::log_info!("media control windows complete command={:?} status={}", command, status);
            }
            Ok(status) => {
                crate::log_warn!("media control windows failed command={:?} status={}", command, status);
            }
            Err(e) => {
                crate::log_warn!("media control windows failed command={:?}: {}", command, e);
            }
        }
    }

    pub fn now_playing() -> Option<NowPlayingMetadata> {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod platform {
    use crate::control::messages::MediaCommand;
    use crate::control::messages::NowPlayingMetadata;

    pub fn handle(command: MediaCommand, _argument: i64) {
        crate::log_warn!("media control unsupported on this platform command={:?}", command);
    }

    pub fn now_playing() -> Option<NowPlayingMetadata> {
        None
    }
}
