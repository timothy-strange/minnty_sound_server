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
            MediaCommand::Play => {
                if read_playback_status(&proxy)? == PlaybackStatus::Playing {
                    crate::log_info!("media control linux ignoring Play because player is already playing player={}", player);
                } else {
                    proxy.method_call("org.mpris.MediaPlayer2.Player", "Play", ())?
                }
            }
            MediaCommand::Pause => proxy.method_call("org.mpris.MediaPlayer2.Player", "Pause", ())?,
            MediaCommand::Stop => proxy.method_call("org.mpris.MediaPlayer2.Player", "Stop", ())?,
            MediaCommand::Next => proxy.method_call("org.mpris.MediaPlayer2.Player", "Next", ())?,
            MediaCommand::Previous => proxy.method_call("org.mpris.MediaPlayer2.Player", "Previous", ())?,
            MediaCommand::SeekRelativeMs => {
                let offset_microseconds = argument.saturating_mul(1_000);
                proxy.method_call("org.mpris.MediaPlayer2.Player", "Seek", (offset_microseconds,))?
            }
            MediaCommand::SeekAbsoluteMs => {
                let track_id = read_track_id(&proxy)?.ok_or("no MPRIS track ID available")?;
                let position_microseconds = argument.max(0).saturating_mul(1_000);
                proxy.method_call("org.mpris.MediaPlayer2.Player", "SetPosition", (track_id, position_microseconds))?
            }
            MediaCommand::VolumeUp | MediaCommand::VolumeDown => return Err("volume commands are not MPRIS commands".into()),
        }
        Ok(player)
    }

    fn read_track_id(
        proxy: &dbus::blocking::Proxy<&Connection>,
    ) -> Result<Option<dbus::Path<'static>>, Box<dyn std::error::Error>> {
        let (metadata,): (Variant<Box<dyn RefArg>>,) = proxy.method_call(
            "org.freedesktop.DBus.Properties",
            "Get",
            ("org.mpris.MediaPlayer2.Player", "Metadata"),
        )?;
        let track_id = dbus::arg::cast::<HashMap<String, Variant<Box<dyn RefArg>>>>(&*metadata.0)
            .and_then(|metadata_map| metadata_map.get("mpris:trackid"))
            .and_then(|v| v.0.as_str())
            .map(|s| dbus::Path::from(s.to_string()));
        Ok(track_id)
    }

    fn read_playback_status(
        proxy: &dbus::blocking::Proxy<&Connection>,
    ) -> Result<PlaybackStatus, Box<dyn std::error::Error>> {
        let (status,): (Variant<Box<dyn RefArg>>,) = proxy.method_call(
            "org.freedesktop.DBus.Properties",
            "Get",
            ("org.mpris.MediaPlayer2.Player", "PlaybackStatus"),
        )?;
        Ok(parse_playback_status(status.0.as_str()))
    }

    fn parse_playback_status(status: Option<&str>) -> PlaybackStatus {
        match status {
            Some("Playing") => PlaybackStatus::Playing,
            Some("Paused") => PlaybackStatus::Paused,
            Some("Stopped") => PlaybackStatus::Stopped,
            _ => PlaybackStatus::Unknown,
        }
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

        let playback_status = parse_playback_status(
            all_props
                .get("PlaybackStatus")
                .and_then(|v| v.0.as_str()),
        );

        let position_ms = all_props
            .get("Position")
            .and_then(|v| refarg_microseconds_to_ms(&*v.0));

        let (title, artist, duration_ms, track_id) = match all_props
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
                let duration_ms = metadata_map
                    .get("mpris:length")
                    .and_then(|v| refarg_microseconds_to_ms(&*v.0));
                let track_id = metadata_map
                    .get("mpris:trackid")
                    .and_then(|v| v.0.as_str())
                    .map(str::to_string);
                (title, artist, duration_ms, track_id)
            }
            None => {
                crate::log_warn!("media control metadata decode failed player={}", player);
                (String::new(), String::new(), None, None)
            }
        };

        Ok(Some(NowPlayingMetadata { artist, title, playback_status, position_ms, duration_ms, track_id }))
    }

    fn refarg_microseconds_to_ms(value: &dyn RefArg) -> Option<u64> {
        value
            .as_i64()
            .and_then(|value| (value >= 0).then_some(value as u64))
            .or_else(|| value.as_u64())
            .map(|value| value / 1_000)
    }

    #[cfg(test)]
    mod tests {
        use super::{parse_playback_status, refarg_microseconds_to_ms};
        use crate::control::messages::PlaybackStatus;

        #[test]
        fn refarg_microseconds_to_ms_accepts_signed_and_unsigned_values() {
            assert_eq!(refarg_microseconds_to_ms(&123_456i64), Some(123));
            assert_eq!(refarg_microseconds_to_ms(&407_973_000u64), Some(407_973));
            assert_eq!(refarg_microseconds_to_ms(&-1i64), None);
        }

        #[test]
        fn parse_playback_status_maps_mpris_strings() {
            assert_eq!(parse_playback_status(Some("Playing")), PlaybackStatus::Playing);
            assert_eq!(parse_playback_status(Some("Paused")), PlaybackStatus::Paused);
            assert_eq!(parse_playback_status(Some("Stopped")), PlaybackStatus::Stopped);
            assert_eq!(parse_playback_status(Some("Other")), PlaybackStatus::Unknown);
            assert_eq!(parse_playback_status(None), PlaybackStatus::Unknown);
        }
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
            MediaCommand::Stop | MediaCommand::SeekRelativeMs | MediaCommand::SeekAbsoluteMs | MediaCommand::VolumeUp | MediaCommand::VolumeDown => {
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
