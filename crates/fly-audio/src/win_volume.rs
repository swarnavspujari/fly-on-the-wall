//! Windows-only: read the default render endpoint's master volume + mute.
//!
//! WASAPI loopback records what the system actually plays — if the output is
//! muted or at 0 %, the "them" channel is silence and the user only finds out
//! after the meeting. `AudioCapture::capture_warnings` uses this to warn
//! before/while recording (polled once a second by the status command).

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};

/// `(volume_scalar 0..1, muted)` of the default output device, or `None` if
/// anything in the COM chain fails (no default device, service down, …).
pub fn default_output_volume() -> Option<(f32, bool)> {
    unsafe {
        // Fine if COM is already initialized on this thread in another mode:
        // we only need it to not be torn down while we hold the interfaces.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        let scalar = volume.GetMasterVolumeLevelScalar().ok()?;
        let muted = volume.GetMute().ok()?.as_bool();
        Some((scalar, muted))
    }
}
