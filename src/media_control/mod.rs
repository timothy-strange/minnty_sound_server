use crate::control::messages::MediaCommand;

pub trait MediaController: Send + Sync {
    fn handle(&self, command: MediaCommand, argument: i64);
}

pub struct PlatformMediaController;

impl MediaController for PlatformMediaController {
    fn handle(&self, command: MediaCommand, argument: i64) {
        platform::handle(command, argument);
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use crate::control::messages::MediaCommand;
    use std::process::Command;

    pub fn handle(command: MediaCommand, argument: i64) {
        let result = match command {
            MediaCommand::PlayPause => Command::new("playerctl").arg("play-pause").status(),
            MediaCommand::Play => Command::new("playerctl").arg("play").status(),
            MediaCommand::Pause => Command::new("playerctl").arg("pause").status(),
            MediaCommand::Next => Command::new("playerctl").arg("next").status(),
            MediaCommand::Previous => Command::new("playerctl").arg("previous").status(),
            MediaCommand::SeekRelativeMs => {
                let seconds = argument as f64 / 1_000.0;
                Command::new("playerctl")
                    .arg("position")
                    .arg(format!("{seconds:+}"))
                    .status()
            }
            MediaCommand::VolumeUp | MediaCommand::VolumeDown => return,
        };

        if let Err(e) = result {
            crate::log_warn!("media control failed command={:?}: {}", command, e);
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use crate::control::messages::MediaCommand;
    use std::process::Command;

    pub fn handle(command: MediaCommand, _argument: i64) {
        let key = match command {
            MediaCommand::PlayPause | MediaCommand::Play | MediaCommand::Pause => 0xB3u8,
            MediaCommand::Next => 0xB0u8,
            MediaCommand::Previous => 0xB1u8,
            MediaCommand::SeekRelativeMs | MediaCommand::VolumeUp | MediaCommand::VolumeDown => return,
        };
        let script = format!(
            "Add-Type -MemberDefinition '[DllImport(\"user32.dll\")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);' -Name Native -Namespace Win32; [Win32.Native]::keybd_event({key},0,0,[UIntPtr]::Zero); [Win32.Native]::keybd_event({key},0,2,[UIntPtr]::Zero)"
        );
        if let Err(e) = Command::new("powershell")
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(script)
            .status()
        {
            crate::log_warn!("media control failed command={:?}: {}", command, e);
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod platform {
    use crate::control::messages::MediaCommand;

    pub fn handle(command: MediaCommand, _argument: i64) {
        crate::log_warn!("media control unsupported on this platform command={:?}", command);
    }
}
