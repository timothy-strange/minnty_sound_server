use crate::control::messages::{MediaCommand, NowPlayingMetadata};

pub trait MediaController: Send + Sync {
    fn handle(&self, command: MediaCommand, argument: i64);
    fn adjust_volume(&self, command: MediaCommand, sink: Option<&str>);
    fn now_playing(&self) -> Option<NowPlayingMetadata>;
}

pub struct PlatformMediaController;

impl MediaController for PlatformMediaController {
    fn handle(&self, command: MediaCommand, argument: i64) {
        platform::handle(command, argument);
    }

    fn adjust_volume(&self, command: MediaCommand, sink: Option<&str>) {
        platform::adjust_volume(command, sink);
    }

    fn now_playing(&self) -> Option<NowPlayingMetadata> {
        platform::now_playing()
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use crate::control::messages::{MediaCommand, NowPlayingMetadata, PlaybackStatus};
    use dbus::arg::{RefArg, Variant};
    use dbus::blocking::Connection;
    use std::collections::HashMap;
    use std::time::Duration;

    pub fn handle(command: MediaCommand, argument: i64) {
        crate::log_info!(
            "media control linux command={:?} argument={}",
            command,
            argument
        );
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

    pub fn adjust_volume(command: MediaCommand, sink: Option<&str>) {
        let step = match command {
            MediaCommand::VolumeUp => "+5%",
            MediaCommand::VolumeDown => "-5%",
            _ => return,
        };
        let target = sink.unwrap_or("@DEFAULT_SINK@");
        match std::process::Command::new("pactl")
            .arg("set-sink-volume")
            .arg(target)
            .arg(step)
            .status()
        {
            Ok(status) if status.success() => {
                crate::log_info!(
                    "server volume adjusted command={:?} sink={}",
                    command,
                    target
                );
            }
            Ok(status) => {
                crate::log_warn!(
                    "server volume adjust failed command={:?} sink={} status={}",
                    command,
                    target,
                    status
                );
            }
            Err(e) => {
                crate::log_warn!(
                    "server volume adjust failed command={:?} sink={}: {}",
                    command,
                    target,
                    e
                );
            }
        }
    }

    fn dispatch_mpris(
        command: MediaCommand,
        argument: i64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let connection = Connection::new_session()?;
        let player = find_player(&connection)?.name;
        let proxy = connection.with_proxy(
            player.as_str(),
            "/org/mpris/MediaPlayer2",
            Duration::from_secs(2),
        );

        match command {
            MediaCommand::PlayPause => {
                proxy.method_call("org.mpris.MediaPlayer2.Player", "PlayPause", ())?
            }
            MediaCommand::Play => {
                if read_playback_status(&proxy)? == PlaybackStatus::Playing {
                    crate::log_info!(
                        "media control linux ignoring Play because player is already playing player={}",
                        player
                    );
                } else {
                    proxy.method_call("org.mpris.MediaPlayer2.Player", "Play", ())?
                }
            }
            MediaCommand::Pause => {
                proxy.method_call("org.mpris.MediaPlayer2.Player", "Pause", ())?
            }
            MediaCommand::Stop => proxy.method_call("org.mpris.MediaPlayer2.Player", "Stop", ())?,
            MediaCommand::Next => proxy.method_call("org.mpris.MediaPlayer2.Player", "Next", ())?,
            MediaCommand::Previous => {
                proxy.method_call("org.mpris.MediaPlayer2.Player", "Previous", ())?
            }
            MediaCommand::SeekRelativeMs => {
                let offset_microseconds = argument.saturating_mul(1_000);
                proxy.method_call(
                    "org.mpris.MediaPlayer2.Player",
                    "Seek",
                    (offset_microseconds,),
                )?
            }
            MediaCommand::SeekAbsoluteMs => {
                let track_id = read_track_id(&proxy)?.ok_or("no MPRIS track ID available")?;
                let position_microseconds = argument.max(0).saturating_mul(1_000);
                proxy.method_call(
                    "org.mpris.MediaPlayer2.Player",
                    "SetPosition",
                    (track_id, position_microseconds),
                )?
            }
            MediaCommand::VolumeUp | MediaCommand::VolumeDown => {
                return Err("volume commands are not MPRIS commands".into());
            }
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

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct PlayerCandidate {
        name: String,
        playback_status: PlaybackStatus,
        has_metadata: bool,
    }

    fn find_player(connection: &Connection) -> Result<PlayerCandidate, Box<dyn std::error::Error>> {
        let dbus = connection.with_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            Duration::from_secs(2),
        );
        let (names,): (Vec<String>,) = dbus.method_call("org.freedesktop.DBus", "ListNames", ())?;
        select_player(
            names
                .into_iter()
                .filter(|name| name.starts_with("org.mpris.MediaPlayer2."))
                .map(|name| read_player_candidate(connection, name))
                .collect::<Vec<_>>(),
        )
        .ok_or_else(|| "no MPRIS media players found".into())
    }

    fn read_player_candidate(connection: &Connection, name: String) -> PlayerCandidate {
        let proxy = connection.with_proxy(
            name.as_str(),
            "/org/mpris/MediaPlayer2",
            Duration::from_secs(2),
        );
        let all_props: Result<(HashMap<String, Variant<Box<dyn RefArg>>>,), _> = proxy.method_call(
            "org.freedesktop.DBus.Properties",
            "GetAll",
            ("org.mpris.MediaPlayer2.Player",),
        );
        let Ok((all_props,)) = all_props else {
            return PlayerCandidate {
                name,
                playback_status: PlaybackStatus::Unknown,
                has_metadata: false,
            };
        };
        PlayerCandidate {
            name,
            playback_status: parse_playback_status(
                all_props.get("PlaybackStatus").and_then(|v| v.0.as_str()),
            ),
            has_metadata: all_props
                .get("Metadata")
                .and_then(|v| dbus::arg::cast::<HashMap<String, Variant<Box<dyn RefArg>>>>(&*v.0))
                .map(metadata_has_display_text)
                .unwrap_or(false),
        }
    }

    fn metadata_has_display_text(metadata: &HashMap<String, Variant<Box<dyn RefArg>>>) -> bool {
        metadata
            .get("xesam:title")
            .and_then(|v| v.0.as_str())
            .is_some_and(|title| !title.trim().is_empty())
            || metadata
                .get("xesam:artist")
                .and_then(|v| v.0.as_iter())
                .and_then(|mut it| it.next().and_then(|v| v.as_str()))
                .is_some_and(|artist| !artist.trim().is_empty())
    }

    fn select_player(candidates: Vec<PlayerCandidate>) -> Option<PlayerCandidate> {
        candidates
            .into_iter()
            .max_by_key(|candidate| player_selection_score(candidate))
    }

    fn player_selection_score(candidate: &PlayerCandidate) -> (u8, bool) {
        let status_score = match candidate.playback_status {
            PlaybackStatus::Playing => 3,
            PlaybackStatus::Paused => 2,
            PlaybackStatus::Stopped => 1,
            PlaybackStatus::Unknown => 0,
        };
        (status_score, candidate.has_metadata)
    }

    pub fn now_playing() -> Option<NowPlayingMetadata> {
        read_now_playing().ok().flatten()
    }

    fn read_now_playing() -> Result<Option<NowPlayingMetadata>, Box<dyn std::error::Error>> {
        let connection = Connection::new_session()?;
        let player = find_player(&connection)?.name;
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

        let playback_status =
            parse_playback_status(all_props.get("PlaybackStatus").and_then(|v| v.0.as_str()));

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

        Ok(Some(NowPlayingMetadata {
            artist,
            title,
            playback_status,
            position_ms,
            duration_ms,
            track_id,
        }))
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
        use super::{
            PlayerCandidate, parse_playback_status, refarg_microseconds_to_ms, select_player,
        };
        use crate::control::messages::PlaybackStatus;

        #[test]
        fn refarg_microseconds_to_ms_accepts_signed_and_unsigned_values() {
            assert_eq!(refarg_microseconds_to_ms(&123_456i64), Some(123));
            assert_eq!(refarg_microseconds_to_ms(&407_973_000u64), Some(407_973));
            assert_eq!(refarg_microseconds_to_ms(&-1i64), None);
        }

        #[test]
        fn parse_playback_status_maps_mpris_strings() {
            assert_eq!(
                parse_playback_status(Some("Playing")),
                PlaybackStatus::Playing
            );
            assert_eq!(
                parse_playback_status(Some("Paused")),
                PlaybackStatus::Paused
            );
            assert_eq!(
                parse_playback_status(Some("Stopped")),
                PlaybackStatus::Stopped
            );
            assert_eq!(
                parse_playback_status(Some("Other")),
                PlaybackStatus::Unknown
            );
            assert_eq!(parse_playback_status(None), PlaybackStatus::Unknown);
        }

        #[test]
        fn select_player_prefers_playing_player_over_paused_player() {
            let selected = select_player(vec![
                candidate(
                    "org.mpris.MediaPlayer2.firefox",
                    PlaybackStatus::Paused,
                    true,
                ),
                candidate(
                    "org.mpris.MediaPlayer2.spotify",
                    PlaybackStatus::Playing,
                    true,
                ),
            ])
            .unwrap();

            assert_eq!(selected.name, "org.mpris.MediaPlayer2.spotify");
        }

        #[test]
        fn select_player_prefers_paused_player_when_none_are_playing() {
            let selected = select_player(vec![
                candidate(
                    "org.mpris.MediaPlayer2.firefox",
                    PlaybackStatus::Stopped,
                    true,
                ),
                candidate(
                    "org.mpris.MediaPlayer2.spotify",
                    PlaybackStatus::Paused,
                    true,
                ),
            ])
            .unwrap();

            assert_eq!(selected.name, "org.mpris.MediaPlayer2.spotify");
        }

        #[test]
        fn select_player_prefers_metadata_with_same_status() {
            let selected = select_player(vec![
                candidate(
                    "org.mpris.MediaPlayer2.empty",
                    PlaybackStatus::Playing,
                    false,
                ),
                candidate(
                    "org.mpris.MediaPlayer2.spotify",
                    PlaybackStatus::Playing,
                    true,
                ),
            ])
            .unwrap();

            assert_eq!(selected.name, "org.mpris.MediaPlayer2.spotify");
        }

        #[test]
        fn select_player_returns_none_for_no_candidates() {
            assert!(select_player(Vec::new()).is_none());
        }

        fn candidate(
            name: &str,
            playback_status: PlaybackStatus,
            has_metadata: bool,
        ) -> PlayerCandidate {
            PlayerCandidate {
                name: name.to_string(),
                playback_status,
                has_metadata,
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use crate::control::messages::{MediaCommand, NowPlayingMetadata, PlaybackStatus};
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    };

    pub fn handle(command: MediaCommand, argument: i64) {
        crate::log_info!(
            "media control windows command={:?} argument={}",
            command,
            argument
        );
        match dispatch_gsmtc(command, argument) {
            Ok(handled) => crate::log_info!(
                "media control windows complete command={:?} handled={}",
                command,
                handled
            ),
            Err(e) => crate::log_warn!("media control windows failed command={:?}: {}", command, e),
        }
    }

    fn dispatch_gsmtc(command: MediaCommand, argument: i64) -> windows::core::Result<bool> {
        let session = current_session()?;
        match command {
            MediaCommand::PlayPause => session.TryTogglePlayPauseAsync()?.join(),
            MediaCommand::Play => session.TryPlayAsync()?.join(),
            MediaCommand::Pause => session.TryPauseAsync()?.join(),
            MediaCommand::Stop => session.TryStopAsync()?.join(),
            MediaCommand::Next => session.TrySkipNextAsync()?.join(),
            MediaCommand::Previous => session.TrySkipPreviousAsync()?.join(),
            MediaCommand::SeekRelativeMs => {
                let timeline = session.GetTimelineProperties()?;
                let current = timeline.Position()?.Duration;
                let target = current
                    .saturating_add(argument.saturating_mul(10_000))
                    .max(0);
                session.TryChangePlaybackPositionAsync(target)?.join()
            }
            MediaCommand::SeekAbsoluteMs => {
                let target = argument.max(0).saturating_mul(10_000);
                session.TryChangePlaybackPositionAsync(target)?.join()
            }
            MediaCommand::VolumeUp | MediaCommand::VolumeDown => Ok(false),
        }
    }

    pub fn adjust_volume(command: MediaCommand, _sink: Option<&str>) {
        crate::log_warn!(
            "server volume adjustment unsupported on Windows command={:?}",
            command
        );
    }

    pub fn now_playing() -> Option<NowPlayingMetadata> {
        read_now_playing().ok()
    }

    fn read_now_playing() -> windows::core::Result<NowPlayingMetadata> {
        let session = current_session()?;
        let media = session.TryGetMediaPropertiesAsync()?.join()?;
        let timeline = session.GetTimelineProperties()?;
        let playback = session.GetPlaybackInfo()?;

        let title = media.Title()?.to_string_lossy().trim().to_string();
        let artist = media.Artist()?.to_string_lossy().trim().to_string();
        let playback_status = match playback.PlaybackStatus()? {
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing => {
                PlaybackStatus::Playing
            }
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused => {
                PlaybackStatus::Paused
            }
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Stopped
            | GlobalSystemMediaTransportControlsSessionPlaybackStatus::Closed => {
                PlaybackStatus::Stopped
            }
            _ => PlaybackStatus::Unknown,
        };

        Ok(NowPlayingMetadata {
            artist,
            title,
            playback_status,
            position_ms: estimated_position_ms(
                timeline.Position()?,
                timeline.LastUpdatedTime()?,
                timeline.StartTime()?,
                timeline.EndTime()?,
                playback_status,
            ),
            duration_ms: duration_ms(timeline.StartTime()?, timeline.EndTime()?),
            track_id: None,
        })
    }

    fn current_session() -> windows::core::Result<GlobalSystemMediaTransportControlsSession> {
        let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.join()?;
        manager.GetCurrentSession()
    }

    fn estimated_position_ms(
        position: windows::Foundation::TimeSpan,
        last_updated: windows::Foundation::DateTime,
        start: windows::Foundation::TimeSpan,
        end: windows::Foundation::TimeSpan,
        playback_status: PlaybackStatus,
    ) -> Option<u64> {
        let mut position_100ns = position.Duration;
        if playback_status == PlaybackStatus::Playing {
            let elapsed_100ns = windows_now_100ns().saturating_sub(last_updated.UniversalTime);
            position_100ns = position_100ns.saturating_add(elapsed_100ns.max(0));
        }
        if end.Duration > start.Duration {
            position_100ns = position_100ns.min(end.Duration - start.Duration);
        }
        (position_100ns >= 0).then_some(position_100ns as u64 / 10_000)
    }

    fn duration_ms(
        start: windows::Foundation::TimeSpan,
        end: windows::Foundation::TimeSpan,
    ) -> Option<u64> {
        (end.Duration > start.Duration).then_some((end.Duration - start.Duration) as u64 / 10_000)
    }

    fn windows_now_100ns() -> i64 {
        const WINDOWS_TO_UNIX_EPOCH_100NS: i64 = 116_444_736_000_000_000;
        let unix_100ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            / 100;
        WINDOWS_TO_UNIX_EPOCH_100NS.saturating_add(unix_100ns.min(i64::MAX as u128) as i64)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod platform {
    use crate::control::messages::MediaCommand;
    use crate::control::messages::NowPlayingMetadata;

    pub fn handle(command: MediaCommand, _argument: i64) {
        crate::log_warn!(
            "media control unsupported on this platform command={:?}",
            command
        );
    }

    pub fn adjust_volume(command: MediaCommand, _sink: Option<&str>) {
        crate::log_warn!(
            "server volume adjustment unsupported on this platform command={:?}",
            command
        );
    }

    pub fn now_playing() -> Option<NowPlayingMetadata> {
        None
    }
}
