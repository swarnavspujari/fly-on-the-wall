//! macOS-only: system-audio loopback via Core Audio process taps
//! (macOS 14.2+). A global tap captures the mix of every process's output;
//! an aggregate device wraps the default output device (main sub-device)
//! plus the tap (sub-tap, auto-started), and an IOProc on that aggregate
//! receives the tapped audio. This is the audio-only path docs/PORTING.md
//! prescribes — `AVAudioEngine` cannot be retargeted to a tap-backed
//! aggregate, so the IOProc is installed with
//! `AudioDeviceCreateIOProcIDWithBlock` directly.
//!
//! Mirrors the Pulse/WASAPI loopback discipline in `pulse_loopback` /
//! `cpal_backend`: mono i16 WAV at the tap's native rate, pad-to-clock so
//! the "them" channel timeline stays wall-clock aligned, and pause
//! implemented by discarding while the shared clock is stopped.
//!
//! The tap needs user consent (NSAudioCaptureUsageDescription, TCC "System
//! Audio Recording Only") and a SIGNED binary — an unsigned/un-entitled
//! build gets callbacks that deliver only zeros. That state is tracked
//! (`nonzero_samples`) and logged at shutdown; the capture itself still
//! degrades gracefully upstream (warning banner + mic-only) whenever the
//! tap cannot be built at all (macOS < 14.2, permission machinery failing).
//!
//! The two tap entry points are 14.2+ symbols resolved with `dlsym` at
//! runtime — the app's deployment target is macOS 12.0, and a hard link
//! reference would keep the whole binary from launching there.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use dispatch2::DispatchQueue;
use objc2::runtime::AnyClass;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioTapPropertyFormat,
    AudioDeviceCreateIOProcIDWithBlock, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID,
    AudioDeviceStart, AudioDeviceStop, AudioHardwareDestroyAggregateDevice,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_foundation::NSArray;

use crate::cpal_backend::{Clock, SharedWriter};
use crate::{AudioError, Result};

/// `kAudioFormatFlagIsFloat` (CoreAudioBaseTypes.h).
const FORMAT_FLAG_IS_FLOAT: u32 = 1;

type CreateTapFn = unsafe extern "C-unwind" fn(*const CATapDescription, *mut AudioObjectID) -> i32;
type DestroyTapFn = unsafe extern "C-unwind" fn(AudioObjectID) -> i32;

/// The IOProc block shape `AudioDeviceCreateIOProcIDWithBlock` expects.
type IoBlock = RcBlock<
    dyn Fn(
        NonNull<AudioTimeStamp>,
        NonNull<AudioBufferList>,
        NonNull<AudioTimeStamp>,
        NonNull<AudioBufferList>,
        NonNull<AudioTimeStamp>,
    ),
>;

/// The process-tap entry points, resolved at runtime. `None` before
/// macOS 14.2 — the caller then keeps the mic-only + banner behavior.
fn tap_functions() -> Option<(CreateTapFn, DestroyTapFn)> {
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const std::os::raw::c_char) -> *mut c_void;
    }
    // RTLD_DEFAULT: CoreAudio is already linked (cpal), no dlopen needed.
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    unsafe {
        let create = dlsym(RTLD_DEFAULT, c"AudioHardwareCreateProcessTap".as_ptr());
        let destroy = dlsym(RTLD_DEFAULT, c"AudioHardwareDestroyProcessTap".as_ptr());
        if create.is_null() || destroy.is_null() {
            return None;
        }
        Some((
            std::mem::transmute::<*mut c_void, CreateTapFn>(create),
            std::mem::transmute::<*mut c_void, DestroyTapFn>(destroy),
        ))
    }
}

/// Whether this macOS can tap system audio at all (14.2+: the
/// CATapDescription class and the tap functions exist).
pub(crate) fn supported() -> bool {
    AnyClass::get(c"CATapDescription").is_some() && tap_functions().is_some()
}

fn check(status: i32, what: &str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(AudioError::Backend(format!(
            "{what} failed (OSStatus {status})"
        )))
    }
}

fn addr(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Read a fixed-size property into `out`. Safety: `out` must really be
/// `size_of::<T>()` writable bytes for the property's type.
unsafe fn get_property<T>(object: AudioObjectID, selector: u32) -> Result<T> {
    let address = addr(selector);
    let mut value = std::mem::zeroed::<T>();
    let mut size = std::mem::size_of::<T>() as u32;
    let status = AudioObjectGetPropertyData(
        object,
        NonNull::from(&address),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new_unchecked(&mut value as *mut T as *mut c_void),
    );
    check(status, "AudioObjectGetPropertyData")?;
    Ok(value)
}

/// Live health probe for the tap, readable from the session while the audio
/// thread owns the recorder. Detects the signed-binary / consent hazard —
/// the IOProc firing, the output device rendering for some process, yet
/// every delivered sample digital zero — so the person recording learns
/// DURING the meeting that the far end is missing, not after.
#[derive(Clone)]
pub(crate) struct TapHealth {
    written: Arc<AtomicU64>,
    nonzero: Arc<AtomicU64>,
    rate: u32,
    output_device: AudioObjectID,
}

impl TapHealth {
    /// Some(warning) once ≥5 s of tap timeline exist with zero non-silent
    /// samples WHILE the output device is rendering somewhere. The
    /// running-somewhere gate keeps quiet in-person meetings (nothing
    /// playing at all — silence is correct) from tripping it; any real
    /// sample ever captured disarms it for good.
    pub(crate) fn silence_warning(&self) -> Option<String> {
        if self.nonzero.load(Ordering::Relaxed) > 0
            || self.written.load(Ordering::Relaxed) < 5 * self.rate as u64
        {
            return None;
        }
        let running: u32 = unsafe {
            get_property(
                self.output_device,
                kAudioDevicePropertyDeviceIsRunningSomewhere,
            )
            .unwrap_or(0)
        };
        (running != 0).then(|| {
            "System audio is playing but its capture is recording only silence — macOS \
             hasn't granted this build system-audio recording permission (unsigned dev \
             builds always record silence; grant it in System Settings → Privacy & \
             Security → Screen & System Audio Recording). The other participants may be \
             missing from this recording."
                .to_string()
        })
    }
}

pub struct TapRecorder {
    pub writer: SharedWriter,
    pub path: PathBuf,
    pub rate: u32,
    pub written: Arc<AtomicU64>,
    /// Samples that were not digital silence — an unsigned/un-entitled
    /// build's tap delivers only zeros, and this is the evidence.
    nonzero: Arc<AtomicU64>,
    /// The IOProc ever ran at all (distinguishes "no callbacks" from
    /// "callbacks full of silence" in the shutdown log line).
    callbacks: Arc<AtomicU64>,
    tap_id: AudioObjectID,
    aggregate_id: AudioObjectID,
    default_output: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    destroy_tap: DestroyTapFn,
    stopped: AtomicBool,
    // Held so the block and its queue outlive the IOProc; dropped last.
    _io_block: IoBlock,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
}

impl TapRecorder {
    /// Build the tap → aggregate → IOProc chain and start capturing into
    /// `path`. Fails cleanly (mic-only fallback + banner upstream) on
    /// macOS < 14.2 or when any Core Audio step refuses.
    pub fn start(path: PathBuf, clock: Arc<Clock>) -> Result<Self> {
        if AnyClass::get(c"CATapDescription").is_none() {
            return Err(AudioError::LoopbackUnsupported);
        }
        let (create_tap, destroy_tap) = tap_functions().ok_or(AudioError::LoopbackUnsupported)?;

        // ---- process tap: everything every process plays, mixed to stereo ----
        let description = unsafe {
            CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &NSArray::new(),
            )
        };
        unsafe { description.setPrivate(true) };
        let tap_uuid = unsafe { description.UUID().UUIDString().to_string() };

        let mut tap_id: AudioObjectID = 0;
        check(
            unsafe { create_tap(&*description, &mut tap_id) },
            "AudioHardwareCreateProcessTap",
        )?;

        // Everything below must destroy the tap on failure — wrap the rest.
        let built = Self::build_on_tap(path, clock, tap_id, tap_uuid, destroy_tap);
        if built.is_err() {
            unsafe {
                let _ = destroy_tap(tap_id);
            }
        }
        built
    }

    fn build_on_tap(
        path: PathBuf,
        clock: Arc<Clock>,
        tap_id: AudioObjectID,
        tap_uuid: String,
        destroy_tap: DestroyTapFn,
    ) -> Result<Self> {
        // ---- tap format: the IOProc will deliver this (Float32 LPCM) ----
        let format: AudioStreamBasicDescription =
            unsafe { get_property(tap_id, kAudioTapPropertyFormat) }?;
        let rate = format.mSampleRate as u32;
        let channels = format.mChannelsPerFrame.max(1) as usize;
        if rate == 0 || format.mFormatFlags & FORMAT_FLAG_IS_FLOAT == 0 {
            return Err(AudioError::Backend(format!(
                "unexpected tap format (rate {rate}, flags {:#x})",
                format.mFormatFlags
            )));
        }
        let interleaved = format.mBytesPerFrame >= (4 * channels) as u32;

        // ---- aggregate device: default output as main sub-device + the tap ----
        let default_output: AudioObjectID = unsafe {
            get_property(
                kAudioObjectSystemObject as AudioObjectID,
                kAudioHardwarePropertyDefaultOutputDevice,
            )
        }?;
        let output_uid = unsafe {
            let uid_ref: core_foundation::string::CFStringRef =
                get_property(default_output, kAudioDevicePropertyDeviceUID)?;
            if uid_ref.is_null() {
                return Err(AudioError::Backend("default output has no UID".into()));
            }
            CFString::wrap_under_create_rule(uid_ref).to_string()
        };

        // Keys are the string values of the kAudioAggregateDevice…/kAudioSub…
        // constants (AudioHardware.h). kAudioAggregateDeviceTapAutoStartKey
        // spares an explicit start ordering; drift compensation on the sub-tap
        // keeps it aligned with the output device's clock.
        let cf_pair = |k: &str, v: CFString| (CFString::new(k).as_CFType(), v.as_CFType());
        let sub_device =
            CFDictionary::from_CFType_pairs(&[cf_pair("uid", CFString::new(&output_uid))]);
        let sub_tap = CFDictionary::from_CFType_pairs(&[
            cf_pair("uid", CFString::new(&tap_uuid)),
            (
                CFString::new("drift").as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
        ]);
        let aggregate_uid = format!("com.flyonthewall.tap-aggregate-{}", std::process::id());
        let description_dict = CFDictionary::from_CFType_pairs(&[
            cf_pair("uid", CFString::new(&aggregate_uid)),
            cf_pair("name", CFString::new("Fly on the Wall system audio")),
            cf_pair("master", CFString::new(&output_uid)),
            (
                CFString::new("private").as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                CFString::new("stacked").as_CFType(),
                CFBoolean::false_value().as_CFType(),
            ),
            (
                CFString::new("tapautostart").as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                CFString::new("subdevices").as_CFType(),
                CFArray::from_CFTypes(&[sub_device.as_CFType()]).as_CFType(),
            ),
            (
                CFString::new("taps").as_CFType(),
                CFArray::from_CFTypes(&[sub_tap.as_CFType()]).as_CFType(),
            ),
        ]);

        let mut aggregate_id: AudioObjectID = 0;
        check(
            unsafe {
                // toll-free bridge: core-foundation's CFDictionaryRef IS the
                // CFDictionary the generated signature wants.
                objc2_core_audio::AudioHardwareCreateAggregateDevice(
                    &*(description_dict.as_concrete_TypeRef()
                        as *const objc2_core_foundation::CFDictionary),
                    NonNull::from(&mut aggregate_id),
                )
            },
            "AudioHardwareCreateAggregateDevice",
        )?;

        let teardown_aggregate = |err: AudioError| -> AudioError {
            unsafe {
                let _ = AudioHardwareDestroyAggregateDevice(aggregate_id);
            }
            err
        };

        // ---- WAV writer + IOProc ----
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer: SharedWriter = Arc::new(Mutex::new(Some(
            hound::WavWriter::create(&path, spec)
                .map_err(|e| teardown_aggregate(AudioError::Backend(e.to_string())))?,
        )));
        let written = Arc::new(AtomicU64::new(0));
        let nonzero = Arc::new(AtomicU64::new(0));
        let callbacks = Arc::new(AtomicU64::new(0));

        let io_block = {
            let writer = writer.clone();
            let written = written.clone();
            let nonzero = nonzero.clone();
            let callbacks = callbacks.clone();
            let clock = clock.clone();
            RcBlock::new(
                move |_now: NonNull<AudioTimeStamp>,
                      in_data: NonNull<AudioBufferList>,
                      _in_time: NonNull<AudioTimeStamp>,
                      _out_data: NonNull<AudioBufferList>,
                      _out_time: NonNull<AudioTimeStamp>| {
                    callbacks.fetch_add(1, Ordering::Relaxed);
                    // Paused: keep the tap running but drop the audio so the
                    // paused stretch never reaches the file (same discipline
                    // as the Pulse loopback).
                    if !clock.is_running() {
                        return;
                    }
                    let mut guard = writer.lock().unwrap();
                    let Some(w) = guard.as_mut() else { return };
                    unsafe {
                        let abl = in_data.as_ref();
                        let buffers = std::slice::from_raw_parts(
                            abl.mBuffers.as_ptr(),
                            abl.mNumberBuffers.min(8) as usize,
                        );
                        let Some(first) = buffers.first().filter(|b| !b.mData.is_null()) else {
                            return;
                        };
                        let (frames, per_frame) = if interleaved {
                            (first.mDataByteSize as usize / (4 * channels), channels)
                        } else {
                            (first.mDataByteSize as usize / 4, 1)
                        };
                        if frames == 0 {
                            return;
                        }
                        // pad-to-clock: taps go quiet with the render pipeline
                        let expected = clock.elapsed_ms() * rate as u64 / 1000;
                        let have = written.load(Ordering::Relaxed) + frames as u64;
                        if expected > have + rate as u64 / 5 {
                            let pad = expected - have;
                            for _ in 0..pad {
                                let _ = w.write_sample(0i16);
                            }
                            written.fetch_add(pad, Ordering::Relaxed);
                        }
                        let mut had_signal = 0u64;
                        for frame in 0..frames {
                            let mut sum = 0f32;
                            let mut n = 0u32;
                            for b in buffers {
                                if b.mData.is_null() {
                                    continue;
                                }
                                let samples = b.mData as *const f32;
                                for c in 0..per_frame.min(b.mNumberChannels.max(1) as usize) {
                                    sum += *samples.add(frame * per_frame + c);
                                    n += 1;
                                }
                            }
                            let mono = if n > 0 { sum / n as f32 } else { 0.0 };
                            let value = (mono.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                            if value != 0 {
                                had_signal += 1;
                            }
                            let _ = w.write_sample(value);
                        }
                        written.fetch_add(frames as u64, Ordering::Relaxed);
                        if had_signal > 0 {
                            nonzero.fetch_add(had_signal, Ordering::Relaxed);
                        }
                    }
                },
            )
        };

        let queue = DispatchQueue::new("com.flyonthewall.tap-io", None);
        let mut proc_id: AudioDeviceIOProcID = None;
        check(
            unsafe {
                AudioDeviceCreateIOProcIDWithBlock(
                    NonNull::from(&mut proc_id),
                    aggregate_id,
                    Some(&queue),
                    &*io_block as *const _ as *mut _,
                )
            },
            "AudioDeviceCreateIOProcIDWithBlock",
        )
        .map_err(teardown_aggregate)?;

        if let Err(e) = check(
            unsafe { AudioDeviceStart(aggregate_id, proc_id) },
            "AudioDeviceStart",
        ) {
            unsafe {
                let _ = AudioDeviceDestroyIOProcID(aggregate_id, proc_id);
            }
            return Err(teardown_aggregate(e));
        }

        tracing::info!(
            rate,
            channels,
            interleaved,
            tap = tap_id,
            aggregate = aggregate_id,
            "system-audio process tap capturing"
        );
        Ok(Self {
            writer,
            path,
            rate,
            written,
            nonzero,
            callbacks,
            tap_id,
            aggregate_id,
            default_output,
            proc_id,
            destroy_tap,
            stopped: AtomicBool::new(false),
            _io_block: io_block,
            _queue: queue,
        })
    }

    pub(crate) fn health(&self) -> TapHealth {
        TapHealth {
            written: self.written.clone(),
            nonzero: self.nonzero.clone(),
            rate: self.rate,
            output_device: self.default_output,
        }
    }

    /// Stop the IOProc and destroy the aggregate + tap. Also the place the
    /// signed-binary hazard becomes visible: callbacks that ran but never
    /// carried a nonzero sample are exactly what an un-entitled build gets.
    pub fn shutdown(&mut self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe {
            let _ = AudioDeviceStop(self.aggregate_id, self.proc_id);
            let _ = AudioDeviceDestroyIOProcID(self.aggregate_id, self.proc_id);
            let _ = AudioHardwareDestroyAggregateDevice(self.aggregate_id);
            let _ = (self.destroy_tap)(self.tap_id);
        }
        let callbacks = self.callbacks.load(Ordering::Relaxed);
        let written = self.written.load(Ordering::Relaxed);
        let nonzero = self.nonzero.load(Ordering::Relaxed);
        if callbacks > 0 && nonzero == 0 {
            tracing::warn!(
                callbacks,
                written,
                "system-audio tap delivered ONLY silence — either nothing \
                 played during the recording, or this build lacks the signed \
                 binary + audio-capture consent the tap needs (see \
                 docs/PORTING.md)"
            );
        } else {
            tracing::info!(callbacks, written, nonzero, "system-audio tap stopped");
        }
    }

    pub fn pad_tail_to(&self, target_ms: u64) {
        let expected = target_ms * self.rate as u64 / 1000;
        let have = self.written.load(Ordering::Relaxed);
        if expected > have {
            if let Some(w) = self.writer.lock().unwrap().as_mut() {
                for _ in 0..(expected - have) {
                    let _ = w.write_sample(0i16);
                }
            }
            self.written.fetch_add(expected - have, Ordering::Relaxed);
        }
    }
}

impl Drop for TapRecorder {
    fn drop(&mut self) {
        self.shutdown();
    }
}
