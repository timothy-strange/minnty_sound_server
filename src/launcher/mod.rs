#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub fn run_launcher() -> Result<(), Box<dyn std::error::Error>> {
    linux::run_launcher()
}

#[cfg(target_os = "windows")]
pub fn run_launcher() -> Result<(), Box<dyn std::error::Error>> {
    windows::run_launcher()
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn run_launcher() -> Result<(), Box<dyn std::error::Error>> {
    Err("launcher is not implemented for this platform".into())
}
