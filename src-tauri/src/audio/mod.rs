use crate::{AudioInputDevice, AudioOutputDevice, PlaybackSession};
use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{SampleFormat, WavSpec, WavWriter};
use rodio::{Decoder, OutputStream, Sink};
use std::collections::VecDeque;
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct CaptureBuffers {
    pub(crate) microphone: Vec<f32>,
    pub(crate) system: Vec<f32>,
}

pub(crate) type IncrementalWavWriter = WavWriter<BufWriter<fs::File>>;

#[derive(Clone, Copy)]
enum AudioSource {
    Microphone,
    System,
}

/// Devices and capture mode resolved on the audio thread, returned to
/// `start_recording` so it can persist the meeting metadata. Every field is
/// `Send`, unlike the CPAL streams that stay behind on the audio thread.
#[derive(Default)]
pub(crate) struct AudioSetup {
    pub(crate) resolved_audio_device_id: Option<String>,
    pub(crate) audio_device_name: Option<String>,
    pub(crate) resolved_system_audio_device_id: Option<String>,
    pub(crate) system_audio_device_name: Option<String>,
    pub(crate) resolved_capture_mode: String,
}

fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    let channel_count = usize::from(channels.max(1));
    if channel_count == 1 {
        return samples.to_vec();
    }
    samples
        .chunks(channel_count)
        .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
        .collect()
}

/// Integer phase survives callbacks: output length is floor(total_input *
/// 16000 / source_rate), independent of callback boundaries. This retains the
/// inexpensive sample-and-hold conversion, not a new anti-aliasing filter.
struct StreamingResampler {
    source_rate: u64,
    phase: u64,
}

impl StreamingResampler {
    fn new(source_rate: u32) -> Self {
        Self { source_rate: u64::from(source_rate.max(1)), phase: 0 }
    }

    fn push(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::new();
        for &sample in samples {
            self.phase += 16_000;
            while self.phase >= self.source_rate {
                output.push(sample);
                self.phase -= self.source_rate;
            }
        }
        output
    }
}

// Allow 250ms of source skew. Beyond this, commit silence for the missing
// source; late samples are discarded at their original indices, never shifted.
const MIX_ALIGNMENT_SAMPLES: u64 = 4_000;
const MAX_CAPTURE_SOURCE_SAMPLES: usize = 32_000;
const MAX_PENDING_SAMPLES: usize = 16_000 * 30;
const MAX_MIX_CHUNK_SAMPLES: u64 = 16_000;
// Absorb timestamp/resampler rounding only, not callback scheduling jitter.
const TIMESTAMP_ROUNDING_SAMPLES: u64 = 2;

// Device timestamps must not manufacture audio ahead of real capture time.
const CAPTURE_CLOCK_TOLERANCE: Duration = Duration::from_millis(250);

/// WASAPI GetPosition can return a zero/stale callback timestamp at startup,
/// even though GetBuffer supplies a valid QPC capture timestamp. Use the later
/// timestamp for ONE shared anchor; never treat callback=0 as system boot time.
struct CaptureClock {
    started_at: Instant,
    anchor: Option<(cpal::StreamInstant, Duration)>,
}

fn capture_clock_elapsed(
    anchor_elapsed: Duration,
    forward: Option<Duration>,
    backward: Option<Duration>,
    observed_elapsed: Duration,
) -> Result<Duration, String> {
    let elapsed = if let Some(forward) = forward {
        anchor_elapsed.checked_add(forward)
            .ok_or_else(|| "Audio capture clock is out of range".to_string())?
    } else {
        let backward = backward.ok_or_else(|| "Audio capture clock is invalid".to_string())?;
        if backward > anchor_elapsed.saturating_add(CAPTURE_CLOCK_TOLERANCE) {
            return Err("Audio capture timestamp moved before the recording clock; capture stopped to preserve timing.".into());
        }
        anchor_elapsed.saturating_sub(backward)
    };
    if elapsed > observed_elapsed.saturating_add(CAPTURE_CLOCK_TOLERANCE) {
        return Err("Audio capture timestamp jumped ahead of real time; capture stopped to avoid generating invalid silence.".into());
    }
    Ok(elapsed)
}

impl CaptureClock {
    fn new(started_at: Instant) -> Self {
        Self { started_at, anchor: None }
    }

    fn position(&mut self, timestamp: cpal::InputStreamTimestamp, observed_at: Instant) -> Result<u64, String> {
        let observed_elapsed = observed_at.saturating_duration_since(self.started_at);
        let (anchor, anchor_elapsed) = *self.anchor.get_or_insert_with(|| {
            (timestamp.capture.max(timestamp.callback), observed_elapsed)
        });
        let elapsed = capture_clock_elapsed(
            anchor_elapsed, timestamp.capture.duration_since(&anchor),
            anchor.duration_since(&timestamp.capture), observed_elapsed,
        )?;
        Ok(((elapsed.as_nanos() * 16_000 + 500_000_000) / 1_000_000_000) as u64)
    }
}

struct AudioPacket {
    start: u64,
    samples: VecDeque<f32>,
}

#[derive(Default)]
struct AlignedSource {
    packets: VecDeque<AudioPacket>,
    queued_samples: usize,
    end: u64,
}

impl AlignedSource {
    fn push(&mut self, mut start: u64, samples: &[f32], cursor: u64, muted: bool) -> Result<(), String> {
        if samples.is_empty() { return Ok(()); }
        if self.end != 0 && start.abs_diff(self.end) <= TIMESTAMP_ROUNDING_SAMPLES {
            start = self.end;
        }
        // Never backfill committed silence or overlap an already queued packet.
        // Missing intervals occupy no queue memory, however long the pause.
        let skip = cursor.max(self.end).saturating_sub(start).min(samples.len() as u64) as usize;
        let samples = &samples[skip..];
        if samples.is_empty() { return Ok(()); }
        if self.queued_samples + samples.len() > MAX_CAPTURE_SOURCE_SAMPLES {
            return Err("Audio persistence could not keep up; capture buffer limit reached.".into());
        }
        let start = start + skip as u64;
        self.end = start + samples.len() as u64;
        self.queued_samples += samples.len();
        self.packets.push_back(AudioPacket {
            start,
            samples: samples.iter().map(|&sample| if muted { 0.0 } else { sample }).collect(),
        });
        Ok(())
    }

    fn sample_at(&mut self, index: u64) -> f32 {
        // A committed prefix must never pin the front packet forever. Only
        // remove indices already passed by the mixer, retaining any live tail.
        while let Some(packet) = self.packets.front_mut() {
            let stale = index.saturating_sub(packet.start).min(packet.samples.len() as u64) as usize;
            packet.samples.drain(..stale);
            packet.start += stale as u64;
            self.queued_samples -= stale;
            if !packet.samples.is_empty() { break; }
            self.packets.pop_front();
        }
        let Some(packet) = self.packets.front_mut() else { return 0.0; };
        if packet.start != index { return 0.0; }
        packet.start += 1;
        let sample = packet.samples.pop_front().unwrap_or_default();
        self.queued_samples -= 1;
        if packet.samples.is_empty() { self.packets.pop_front(); }
        sample
    }
}

struct AlignedMixer {
    microphone: AlignedSource,
    system: AlignedSource,
    capture_microphone: bool,
    capture_system: bool,
    cursor: u64,
}

impl AlignedMixer {
    fn new(capture_microphone: bool, capture_system: bool) -> Self {
        Self {
            microphone: AlignedSource::default(), system: AlignedSource::default(),
            capture_microphone, capture_system, cursor: 0,
        }
    }

    fn push(&mut self, source: AudioSource, start: u64, samples: &[f32], muted: bool) -> Result<(), String> {
        match source {
            AudioSource::Microphone => self.microphone.push(start, samples, self.cursor, muted),
            AudioSource::System => self.system.push(start, samples, self.cursor, false),
        }
    }

    fn drain(&mut self, final_chunk: bool) -> Vec<f32> {
        let mic_end = self.microphone.end;
        let system_end = self.system.end;
        let both = self.capture_microphone && self.capture_system;
        let end = if both && !final_chunk {
            mic_end.min(system_end).max(mic_end.max(system_end).saturating_sub(MIX_ALIGNMENT_SAMPLES))
        } else if both {
            mic_end.max(system_end)
        } else if self.capture_microphone {
            mic_end
        } else {
            system_end
        };
        // Long loopback-only pauses must not allocate the whole silent interval.
        let end = end.min(self.cursor.saturating_add(MAX_MIX_CHUNK_SAMPLES));
        let mut mixed = Vec::with_capacity(end.saturating_sub(self.cursor) as usize);
        while self.cursor < end {
            let mic = self.microphone.sample_at(self.cursor);
            let system = self.system.sample_at(self.cursor);
            let value = if both { mic * 0.6 + system * 0.4 }
                else if self.capture_microphone { mic } else { system };
            // Both persistence and inference consume the exact PCM16 values.
            let pcm = pcm16_sample(value);
            mixed.push(pcm as f32 / i16::MAX as f32);
            self.cursor += 1;
        }
        mixed
    }
}

/// Meeting-only capture path. Screen capture keeps its existing raw source
/// buffers/level meters. Only the persistence task drains this mixer.
pub(crate) struct RecordingCapture {
    mixer: AlignedMixer,
    clock: Option<CaptureClock>,
    stopped: bool,
    error: Option<String>,
}

impl RecordingCapture {
    pub(crate) fn new(microphone: bool, system: bool) -> Self {
        Self { mixer: AlignedMixer::new(microphone, system), clock: None, stopped: false, error: None }
    }

    fn start(&mut self, started_at: Instant) -> Result<(), String> {
        if self.stopped || self.clock.is_some() {
            return Err("Audio capture startup was cancelled or already completed".into());
        }
        if let Some(error) = &self.error { return Err(error.clone()); }
        self.clock = Some(CaptureClock::new(started_at));
        Ok(())
    }

    // Freeze admission before draining, including when device teardown hangs.
    // Already accepted packets and the fatal error latch remain untouched.
    pub(crate) fn stop(&mut self) {
        self.stopped = true;
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn drain(&mut self, final_chunk: bool) -> Result<Vec<f32>, String> {
        // Fatal capture errors freeze admission, but accepted packets must still
        // be drainable for recovery/finalization. Never clear the fatal latch.
        if !final_chunk {
            if let Some(error) = &self.error { return Err(error.clone()); }
        }
        Ok(self.mixer.drain(final_chunk))
    }
}

/// A slow/stopped inference worker must not accumulate an entire meeting in
/// RAM. Overflow explicitly ends live transcription, but never stops saving.
#[derive(Default)]
pub(crate) struct PendingAudio {
    samples: Vec<f32>,
    error: Option<String>,
}

impl PendingAudio {
    pub(crate) fn push(&mut self, samples: &[f32]) {
        if self.error.is_some() { return; }
        if self.samples.len() + samples.len() > MAX_PENDING_SAMPLES {
            self.samples.clear();
            self.error = Some("Live transcription fell more than 30 seconds behind; audio is still being saved. Re-transcribe the saved recording.".into());
        } else {
            self.samples.extend_from_slice(samples);
        }
    }

    pub(crate) fn take(&mut self) -> Result<Vec<f32>, String> {
        if let Some(error) = &self.error { return Err(error.clone()); }
        Ok(std::mem::take(&mut self.samples))
    }
}

#[derive(Clone)]
enum CaptureTarget {
    Raw(Arc<Mutex<CaptureBuffers>>, Option<Arc<Mutex<CaptureBuffers>>>),
    Recording(Arc<Mutex<RecordingCapture>>),
}

impl CaptureTarget {
    fn fail(&self, error: String) {
        eprintln!("{error}");
        if let Self::Recording(capture) = self {
            if let Ok(mut capture) = capture.lock() {
                if capture.error.is_none() { capture.error = Some(error); }
            }
        }
    }

    fn append(&self, source: AudioSource, samples: &[f32], muted: bool, timestamp: cpal::InputStreamTimestamp, observed_at: Instant) {
        match self {
            Self::Raw(buffers, pending) => {
                append_capture_samples_with_mute(buffers, source, samples, muted);
                if let Some(pending) = pending {
                    append_capture_samples_with_mute(pending, source, samples, muted);
                }
            }
            Self::Recording(capture) => {
                if let Ok(mut capture) = capture.lock() {
                    if !capture.stopped && capture.error.is_none() {
                        // Constructed streams are paused. No recording clock
                        // exists until the owning thread is about to play them.
                        let Some(clock) = capture.clock.as_mut() else { return; };
                        let result = clock.position(timestamp, observed_at)
                            .and_then(|start| capture.mixer.push(source, start, samples, muted));
                        if let Err(error) = result {
                            capture.error = Some(error);
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn mix_sources(microphone: &[f32], system: &[f32]) -> Vec<f32> {
    if microphone.is_empty() {
        return system.to_vec();
    }
    if system.is_empty() {
        return microphone.to_vec();
    }
    let max_len = microphone.len().max(system.len());
    let mut mixed = Vec::with_capacity(max_len);
    for index in 0..max_len {
        let mic_sample = microphone.get(index).copied().unwrap_or_default();
        let system_sample = system.get(index).copied().unwrap_or_default();
        mixed.push((mic_sample * 0.6 + system_sample * 0.4).clamp(-1.0, 1.0));
    }
    mixed
}

fn append_capture_samples_with_mute(
    buffers: &Arc<Mutex<CaptureBuffers>>,
    source: AudioSource,
    samples: &[f32],
    microphone_muted: bool,
) {
    if let Ok(mut buffers) = buffers.lock() {
        match source {
            AudioSource::Microphone if microphone_muted => {
                buffers
                    .microphone
                    .extend(std::iter::repeat(0.0).take(samples.len()));
            }
            AudioSource::Microphone => buffers.microphone.extend_from_slice(samples),
            AudioSource::System => buffers.system.extend_from_slice(samples),
        }
    }
}

fn convert_samples_i16(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| *sample as f32 / i16::MAX as f32)
        .collect()
}

fn convert_samples_u16(samples: &[u16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
        .collect()
}

fn build_capture_stream(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    source: AudioSource,
    target: CaptureTarget,
    microphone_muted: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let sample_rate = config.sample_rate().0;
    let mut resampler = StreamingResampler::new(sample_rate);
    let channels = config.channels();
    let error_target = target.clone();
    let error_callback = move |error| error_target.fail(format!("Audio stream error: {error}"));

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], info| {
                let observed_at = Instant::now();
                let mono = downmix_to_mono(data, channels);
                let samples_16k = resampler.push(&mono);
                let muted = microphone_muted.load(Ordering::Acquire);
                target.append(source, &samples_16k, muted, info.timestamp(), observed_at);
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], info| {
                let observed_at = Instant::now();
                let converted = convert_samples_i16(data);
                let mono = downmix_to_mono(&converted, channels);
                let samples_16k = resampler.push(&mono);
                let muted = microphone_muted.load(Ordering::Acquire);
                target.append(source, &samples_16k, muted, info.timestamp(), observed_at);
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], info| {
                let observed_at = Instant::now();
                let converted = convert_samples_u16(data);
                let mono = downmix_to_mono(&converted, channels);
                let samples_16k = resampler.push(&mono);
                let muted = microphone_muted.load(Ordering::Acquire);
                target.append(source, &samples_16k, muted, info.timestamp(), observed_at);
            },
            error_callback,
            None,
        ),
        _ => return Err("Unsupported audio sample format".to_string()),
    }
    .map_err(|error| error.to_string())?;

    Ok(stream)
}

fn audio_host() -> cpal::Host {
    cpal::host_from_id(cpal::HostId::Wasapi).unwrap_or_else(|_| cpal::default_host())
}

fn default_audio_input_device(host: &cpal::Host) -> Result<cpal::Device, String> {
    host.default_input_device()
        .ok_or_else(|| "No default microphone found".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn capture_clock_rejects_uptime_silence_but_preserves_real_pauses_and_delayed_packets() {
        let initial = Duration::from_millis(40);
        // A first-packet anchor has no delta, regardless of machine uptime.
        assert_eq!(capture_clock_elapsed(initial, Some(Duration::ZERO), None, initial).unwrap(), initial);
        assert!(capture_clock_elapsed(initial, Some(Duration::from_secs(26_349)), None, initial).is_err());
        let later = Duration::from_secs(65);
        assert_eq!(capture_clock_elapsed(initial, Some(later), None, later + initial).unwrap(), later + initial);
        // Callback delivery delay must NOT shift an old packet to now.
        assert_eq!(capture_clock_elapsed(initial, Some(Duration::from_secs(1)), None, later).unwrap(), Duration::from_millis(1040));
        assert_eq!(capture_clock_elapsed(initial, None, Some(Duration::from_millis(50)), initial).unwrap(), Duration::ZERO);
        assert!(capture_clock_elapsed(initial, None, Some(Duration::from_secs(26_349)), later).is_err());
    }

    #[test]
    #[ignore = "LOCAL LOOPBACK DIAGNOSTIC: plays synthetic tone; records timestamp metadata only, no cloud or files"]
    fn wasapi_timestamp_diagnostic() {
        let host = audio_host();
        let device = host.default_output_device().expect("Default output device");
        let config = device.default_output_config().unwrap();
        assert_eq!(config.sample_format(), cpal::SampleFormat::F32);
        let (tx, rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        let stream = device.build_input_stream(&config.into(), move |_: &[f32], info: &cpal::InputCallbackInfo| {
            let ts = info.timestamp();
            let _ = tx.send((started.elapsed(), ts));
        }, |e| eprintln!("loopback error: {e}"), None).unwrap();
        stream.play().unwrap();
        let (_output, handle) = OutputStream::try_default().unwrap();
        let sink = Sink::try_new(&handle).unwrap();
        use rodio::Source;
        sink.append(rodio::source::SineWave::new(440.0).amplify(0.01).take_duration(Duration::from_secs(2)));
        for _ in 0..8 {
            let (elapsed, timestamp) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            eprintln!("WASAPI elapsed={elapsed:?} capture={:?} callback={:?} capture_after_callback={:?}",
                timestamp.capture, timestamp.callback, timestamp.capture.duration_since(&timestamp.callback));
        }
        sink.stop();
        drop(stream);
    }

    #[test]
    fn slow_preparation_has_no_clock_or_backlog_and_play_uses_one_epoch() {
        let capture = Arc::new(Mutex::new(RecordingCapture::new(true, true)));
        // Simulate preparation lasting far beyond the two-second buffer cap,
        // without sleeping or opening devices. Only play establishes time zero.
        let prepared_at = Instant::now() - Duration::from_secs(60);
        {
            let mut capture = capture.lock().unwrap();
            assert!(capture.clock.is_none());
            assert!(capture.drain(false).unwrap().is_empty());
        }
        let mut played = Vec::new();
        let mut epochs = Vec::new();
        let started_at = start_recording_sources(&["microphone", "system"], &capture, |source| {
            played.push(*source);
            let mut capture = capture.lock().unwrap();
            epochs.push(capture.clock.as_ref().unwrap().started_at);
            if *source == "microphone" {
                capture.mixer.push(AudioSource::Microphone, 0, &[0.5; 320], false).unwrap();
            } else {
                capture.mixer.push(AudioSource::System, 160, &[0.5; 320], false).unwrap();
            }
            Ok(())
        }).unwrap();
        assert_eq!(played, ["microphone", "system"]);
        assert_eq!(epochs, [started_at, started_at]);
        assert!(started_at.duration_since(prepared_at) >= Duration::from_secs(60));
        let mixed = capture.lock().unwrap().drain(true).unwrap();
        assert_eq!(mixed.len(), 480); // No minute of artificial startup silence.
        for (range, value) in [(0..160, 0.3), (160..320, 0.5), (320..480, 0.2)] {
            assert!(mixed[range].iter().all(|&s| pcm16_sample(s) == pcm16_sample(value)));
        }
        assert!(start_recording_sources(&[()], &capture, |_| panic!("must not restart")).is_err());
    }

    #[test]
    fn cancelled_start_never_plays_and_partial_play_failure_keeps_accepted_tail() {
        let capture = Arc::new(Mutex::new(RecordingCapture::new(true, true)));
        capture.lock().unwrap().stop();
        assert!(start_recording_sources(&[()], &capture, |_| panic!("cancelled start played")).is_err());
        assert!(capture.lock().unwrap().clock.is_none());

        let capture = Arc::new(Mutex::new(RecordingCapture::new(true, true)));
        let result = start_recording_sources(&["microphone", "system"], &capture, |source| {
            if *source == "system" { return Err("synthetic play failure".into()); }
            capture.lock().unwrap().mixer.push(AudioSource::Microphone, 0, &[0.5; 320], false)
        });
        assert_eq!(result.unwrap_err(), "synthetic play failure");
        let mut capture = capture.lock().unwrap();
        assert!(capture.stopped);
        let tail = capture.drain(true).unwrap();
        assert_eq!(tail.len(), 320);
        assert!(tail.iter().all(|&s| pcm16_sample(s) == pcm16_sample(0.3)));
    }

    #[test]
    fn stale_front_packets_release_only_committed_prefix_and_do_not_pin_queue() {
        let mut source = AlignedSource::default();
        source.push(0, &[0.1, 0.2], 0, false).unwrap();
        source.push(5, &[0.3, 0.4, 0.5], 0, false).unwrap();
        // Defensive invariant test: simulate a cursor already beyond a queued
        // packet. Normal push/drain currently prevents this state.
        assert_eq!(source.sample_at(6), 0.4);
        assert_eq!(source.queued_samples, 1);
        assert_eq!(source.sample_at(7), 0.5);
        assert_eq!(source.queued_samples, 0);
        assert!(source.packets.is_empty());
        source.push(12, &[0.75; MAX_CAPTURE_SOURCE_SAMPLES], 8, false).unwrap();
        assert_eq!(source.sample_at(8), 0.0); // Future audio must not shift.
        assert_eq!(source.queued_samples, MAX_CAPTURE_SOURCE_SAMPLES);
        assert_eq!(source.sample_at(12), 0.75);
    }

    #[test]
    fn muted_microphone_samples_preserve_length_as_silence() {
        let buffers = Arc::new(Mutex::new(CaptureBuffers::default()));

        append_capture_samples_with_mute(
            &buffers,
            AudioSource::Microphone,
            &[0.25, -0.5, 0.75],
            true,
        );

        let buffers = buffers.lock().unwrap();
        assert_eq!(buffers.microphone, vec![0.0, 0.0, 0.0]);
        assert!(buffers.system.is_empty());
    }

    #[test]
    fn mute_does_not_change_system_samples() {
        let buffers = Arc::new(Mutex::new(CaptureBuffers::default()));

        append_capture_samples_with_mute(&buffers, AudioSource::System, &[0.25, -0.5, 0.75], true);

        let buffers = buffers.lock().unwrap();
        assert_eq!(buffers.system, vec![0.25, -0.5, 0.75]);
        assert!(buffers.microphone.is_empty());
    }

    #[test]
    fn unmuted_microphone_samples_are_unchanged() {
        let buffers = Arc::new(Mutex::new(CaptureBuffers::default()));

        append_capture_samples_with_mute(
            &buffers,
            AudioSource::Microphone,
            &[0.25, -0.5, 0.75],
            false,
        );

        assert_eq!(buffers.lock().unwrap().microphone, vec![0.25, -0.5, 0.75]);
    }

    #[test]
    fn resampling_preserves_fractional_phase_across_arbitrary_callbacks() {
        for rate in [8_000, 16_000, 44_100, 48_000] {
            let input: Vec<f32> = (0..rate * 2).map(|index| (index % 97) as f32 / 97.0).collect();
            let expected = StreamingResampler::new(rate).push(&input);
            assert_eq!(expected.len(), 32_000);
            for callback_size in [1, 127, 441, 512, 1_024] {
                let mut resampler = StreamingResampler::new(rate);
                let actual: Vec<f32> = input.chunks(callback_size)
                    .flat_map(|chunk| resampler.push(chunk)).collect();
                assert_eq!(actual, expected, "rate={rate}, callback_size={callback_size}");
                assert_eq!(resampler.phase, 0);
            }
        }
    }

    #[test]
    fn resampling_does_not_round_each_short_callback_down() {
        let mut resampler = StreamingResampler::new(44_100);
        let mut count = 0;
        for input_count in 1..=44_101_u64 {
            count += resampler.push(&[0.0]).len() as u64;
            assert_eq!(count, input_count * 16_000 / 44_100);
            assert_eq!(resampler.phase, input_count * 16_000 % 44_100);
        }
    }

    #[test]
    fn mixing_is_independent_of_device_callback_and_drain_boundaries() {
        let mic: Vec<f32> = (0..12_000).map(|index| (index % 41) as f32 / 41.0).collect();
        let system: Vec<f32> = (0..12_000).map(|index| -(index % 53) as f32 / 53.0).collect();
        let mut mixer = AlignedMixer::new(true, true);
        let mut actual = Vec::new();
        let mut mic_cursor = 0;
        let mut system_cursor = 0;
        while mic_cursor < mic.len() || system_cursor < system.len() {
            let mic_end = (mic_cursor + 733).min(mic.len());
            mixer.push(AudioSource::Microphone, mic_cursor as u64, &mic[mic_cursor..mic_end], false).unwrap();
            mic_cursor = mic_end;
            actual.extend(mixer.drain(false));
            let system_end = (system_cursor + 701).min(system.len());
            mixer.push(AudioSource::System, system_cursor as u64, &system[system_cursor..system_end], false).unwrap();
            system_cursor = system_end;
            actual.extend(mixer.drain(false));
        }
        actual.extend(mixer.drain(true));
        let expected: Vec<f32> = mic.iter().zip(&system)
            .map(|(mic, system)| pcm16_sample(mic * 0.6 + system * 0.4) as f32 / i16::MAX as f32)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(mixer.cursor, 12_000);
        assert!(mixer.drain(true).is_empty());
    }

    #[test]
    fn missing_source_is_silence_and_late_samples_keep_their_original_indices() {
        let mut mixer = AlignedMixer::new(true, true);
        mixer.push(AudioSource::Microphone, 0, &vec![1.0; 5_000], false).unwrap();
        let first = mixer.drain(false);
        assert_eq!(first.len(), 1_000);
        assert!(first.iter().all(|sample| pcm16_sample(*sample) == pcm16_sample(0.6)));
        // The first 1,000 late samples must not shift into the next interval.
        let mut late = vec![-1.0; 1_000];
        late.extend(vec![1.0; 500]);
        mixer.push(AudioSource::System, 0, &late, false).unwrap();
        assert_eq!(mixer.drain(false), vec![1.0; 500]);
        let tail = mixer.drain(true);
        assert_eq!(tail.len(), 3_500);
        assert!(tail.iter().all(|sample| pcm16_sample(*sample) == pcm16_sample(0.6)));
        assert_eq!(mixer.cursor, 5_000);
    }

    #[test]
    fn muted_recording_keeps_exact_duration_and_system_gain() {
        let mut mixer = AlignedMixer::new(true, true);
        mixer.push(AudioSource::Microphone, 0, &[1.0; 320], true).unwrap();
        mixer.push(AudioSource::System, 0, &[0.5; 320], false).unwrap();
        let mixed = mixer.drain(true);
        assert_eq!(mixed.len(), 320);
        assert!(mixed.iter().all(|sample| pcm16_sample(*sample) == pcm16_sample(0.2)));
    }

    #[test]
    fn pcm16_roundtrip_is_idempotent_for_every_mixer_output() {
        for value in -i16::MAX..=i16::MAX {
            assert_eq!(pcm16_sample(value as f32 / i16::MAX as f32), value);
        }
    }

    #[test]
    fn persisted_pcm_and_pending_transcription_use_the_same_timeline() {
        // Synthetic test file only; never opens any existing meeting recording.
        struct TestWav(std::path::PathBuf);
        impl Drop for TestWav {
            fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
        }
        let file = TestWav(std::env::temp_dir().join(format!("meetly-audio-test-{}.wav", uuid::Uuid::new_v4())));
        let mut writer = create_incremental_wav(&file.0, 16_000).unwrap();
        let mut pending = PendingAudio::default();
        let mut mixer = AlignedMixer::new(true, false);
        for chunk in [&[0.25, -0.75, 0.5][..], &[0.0; 320][..], &[1.0, -1.0][..]] {
            mixer.push(AudioSource::Microphone, mixer.cursor, chunk, false).unwrap();
            let mixed = mixer.drain(false);
            append_wav_chunk(&mut writer, &mixed).unwrap();
            pending.push(&mixed);
        }
        writer.finalize().unwrap();
        let mut reader = hound::WavReader::open(&file.0).unwrap();
        // Inference hasn't consumed anything yet; duration is already final.
        assert_eq!(reader.duration(), 325);
        assert_eq!(reader.spec().sample_rate, 16_000);
        let saved: Vec<i16> = reader.samples::<i16>().map(|sample| sample.unwrap()).collect();
        let samples = pending.take().unwrap();
        assert_eq!(samples.len(), 325);
        let expected: Vec<i16> = samples.iter().copied().map(pcm16_sample).collect();
        assert_eq!(saved, expected);
        let azure_bytes = general_purpose::STANDARD.decode(pcm16_base64(&samples)).unwrap();
        let saved_bytes: Vec<u8> = saved.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        assert_eq!(azure_bytes, saved_bytes);
    }

    #[test]
    fn inference_overflow_is_bounded_and_does_not_stop_the_recording_mixer() {
        let mut pending = PendingAudio::default();
        pending.push(&vec![0.0; MAX_PENDING_SAMPLES]);
        pending.push(&[0.0]);
        assert!(pending.samples.is_empty());
        assert!(pending.take().unwrap_err().contains("still being saved"));
        let mut mixer = AlignedMixer::new(true, false);
        mixer.push(AudioSource::Microphone, 0, &[0.5; 320], false).unwrap();
        assert_eq!(mixer.drain(true).len(), 320);
    }

    #[test]
    fn delayed_loopback_start_joins_current_time_instead_of_staying_late() {
        let mut mixer = AlignedMixer::new(true, true);
        let mut mixed = Vec::new();
        for start in (0..16_000).step_by(320) {
            mixer.push(AudioSource::Microphone, start, &[0.5; 320], false).unwrap();
            // Loopback produces its FIRST samples 500ms after microphone start.
            if start >= 8_000 {
                mixer.push(AudioSource::System, start, &[0.5; 320], false).unwrap();
            }
            mixed.extend(mixer.drain(false));
        }
        mixed.extend(mixer.drain(true));
        assert_eq!(mixed.len(), 16_000);
        assert!(mixed[..8_000].iter().all(|&s| pcm16_sample(s) == pcm16_sample(0.3)));
        assert!(mixed[8_000..].iter().all(|&s| pcm16_sample(s) == pcm16_sample(0.5)));
    }

    #[test]
    fn loopback_pause_resume_preserves_gap_and_never_backfills_committed_silence() {
        let mut mixer = AlignedMixer::new(true, true);
        let mut mixed = Vec::new();
        for start in (0..24_000).step_by(320) {
            mixer.push(AudioSource::Microphone, start, &[0.5; 320], false).unwrap();
            if start < 3_200 || start >= 16_000 {
                mixer.push(AudioSource::System, start, &[0.5; 320], false).unwrap();
            }
            if start == 12_800 {
                // Delivered now, but captured in an already committed interval.
                let cursor = mixer.cursor;
                mixer.push(AudioSource::System, 3_200, &[-1.0; 320], false).unwrap();
                assert_eq!(mixer.cursor, cursor);
                assert_eq!(mixer.system.queued_samples, 0);
            }
            mixed.extend(mixer.drain(false));
        }
        mixed.extend(mixer.drain(true));
        assert_eq!(mixed.len(), 24_000);
        assert!(mixed[..3_200].iter().all(|&s| pcm16_sample(s) == pcm16_sample(0.5)));
        assert!(mixed[3_200..16_000].iter().all(|&s| pcm16_sample(s) == pcm16_sample(0.3)));
        assert!(mixed[16_000..].iter().all(|&s| pcm16_sample(s) == pcm16_sample(0.5)));
    }

    #[test]
    fn timestamp_rounding_is_absorbed_but_early_packets_are_not_shifted_to_now() {
        let mut mixer = AlignedMixer::new(false, true);
        mixer.push(AudioSource::System, 0, &[0.5; 320], false).unwrap();
        mixer.push(AudioSource::System, 319, &[0.5; 320], false).unwrap();
        mixer.push(AudioSource::System, 641, &[0.5; 320], false).unwrap();
        assert_eq!(mixer.drain(false).len(), 960);
        // A genuine overlap larger than rounding tolerance is trimmed, not
        // shifted wholesale to the end of the previous packet.
        mixer.push(AudioSource::System, 800, &[-0.5; 320], false).unwrap();
        assert_eq!(mixer.drain(false).len(), 160);
        assert_eq!(mixer.cursor, 1_120);
    }

    #[test]
    fn system_only_long_pause_is_sparse_but_persisted_as_bounded_silence_chunks() {
        let mut mixer = AlignedMixer::new(false, true);
        mixer.push(AudioSource::System, 0, &[0.5; 320], false).unwrap();
        assert_eq!(mixer.drain(false).len(), 320);
        let resumed_at = 16_000 * 60;
        mixer.push(AudioSource::System, resumed_at, &[0.5; 320], false).unwrap();
        assert_eq!(mixer.system.queued_samples, 320);
        assert_eq!(mixer.system.packets.len(), 1);
        loop {
            let start = mixer.cursor;
            let chunk = mixer.drain(true);
            if chunk.is_empty() { break; }
            assert!(chunk.len() <= MAX_MIX_CHUNK_SAMPLES as usize);
            for (offset, &sample) in chunk.iter().enumerate() {
                let expected = if start + (offset as u64) < resumed_at { 0.0 } else { 0.5 };
                assert_eq!(pcm16_sample(sample), pcm16_sample(expected));
            }
        }
        assert_eq!(mixer.cursor, resumed_at + 320);
    }

    #[test]
    fn capture_overflow_keeps_accepted_packets_available_for_finalization() {
        let mut capture = RecordingCapture::new(true, false);
        capture.mixer.push(AudioSource::Microphone, 0, &vec![0.5; MAX_CAPTURE_SOURCE_SAMPLES], false).unwrap();
        capture.error = capture.mixer.push(
            AudioSource::Microphone, MAX_CAPTURE_SOURCE_SAMPLES as u64, &[1.0; 320], false,
        ).err();
        assert!(capture.drain(false).unwrap_err().contains("buffer limit"));
        assert_eq!(capture.mixer.microphone.queued_samples, MAX_CAPTURE_SOURCE_SAMPLES);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic-overflow-recovery.wav");
        let mut writer = create_incremental_wav(&path, 16_000).unwrap();
        let mut pending = PendingAudio::default();
        let mut recovered = Vec::new();
        loop {
            let chunk = capture.drain(true).unwrap();
            if chunk.is_empty() { break; }
            append_wav_chunk(&mut writer, &chunk).unwrap();
            pending.push(&chunk);
            recovered.extend(chunk);
        }
        writer.finalize().unwrap();
        assert_eq!(recovered.len(), MAX_CAPTURE_SOURCE_SAMPLES);
        assert!(recovered.iter().all(|&s| pcm16_sample(s) == pcm16_sample(0.5)));
        assert_eq!(pending.take().unwrap(), recovered);
        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), MAX_CAPTURE_SOURCE_SAMPLES as u32);
        let saved: Vec<i16> = reader.samples::<i16>().map(|sample| sample.unwrap()).collect();
        assert_eq!(saved, recovered.iter().copied().map(pcm16_sample).collect::<Vec<_>>());
        assert!(capture.error().is_some());
        assert!(capture.drain(false).is_err());
    }
}

fn default_audio_output_device(host: &cpal::Host) -> Result<cpal::Device, String> {
    host.default_output_device()
        .ok_or_else(|| "No default system audio output found".to_string())
}

fn audio_device_name(device: &cpal::Device) -> String {
    device
        .name()
        .unwrap_or_else(|_| "Unknown input".to_string())
}

fn resolve_audio_input_device(
    host: &cpal::Host,
    audio_device_id: Option<&str>,
) -> Result<(cpal::Device, Option<String>, Option<String>), String> {
    let selected_id = audio_device_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(selected_id) = selected_id {
        let devices = host.input_devices().map_err(|error| error.to_string())?;
        for device in devices {
            let name = audio_device_name(&device);
            if name == selected_id {
                return Ok((device, Some(selected_id.to_string()), Some(name)));
            }
        }
    }

    let device = default_audio_input_device(host)?;
    let name = audio_device_name(&device);
    Ok((device, None, Some(name)))
}

fn resolve_audio_output_device(
    host: &cpal::Host,
    system_audio_device_id: Option<&str>,
) -> Result<(cpal::Device, Option<String>, Option<String>), String> {
    let selected_id = system_audio_device_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(selected_id) = selected_id {
        let devices = host.output_devices().map_err(|error| error.to_string())?;
        for device in devices {
            let name = audio_device_name(&device);
            if name == selected_id {
                return Ok((device, Some(selected_id.to_string()), Some(name)));
            }
        }
    }

    let device = default_audio_output_device(host)?;
    let name = audio_device_name(&device);
    Ok((device, None, Some(name)))
}

pub(crate) fn list_input_devices() -> Result<Vec<AudioInputDevice>, String> {
    let host = audio_host();
    let default_name = host
        .default_input_device()
        .map(|device| audio_device_name(&device));
    let mut devices = vec![AudioInputDevice {
        id: String::new(),
        name: "System default".to_string(),
        is_default: true,
    }];

    for device in host.input_devices().map_err(|error| error.to_string())? {
        let name = audio_device_name(&device);
        if devices.iter().any(|item| item.id == name) {
            continue;
        }
        let is_default = default_name.as_deref() == Some(name.as_str());
        devices.push(AudioInputDevice {
            id: name.clone(),
            name,
            is_default,
        });
    }

    Ok(devices)
}

pub(crate) fn list_output_devices() -> Result<Vec<AudioOutputDevice>, String> {
    let host = audio_host();
    let default_name = host
        .default_output_device()
        .map(|device| audio_device_name(&device));
    let mut devices = vec![AudioOutputDevice {
        id: String::new(),
        name: "System default".to_string(),
        is_default: true,
    }];

    for device in host.output_devices().map_err(|error| error.to_string())? {
        let name = audio_device_name(&device);
        if devices.iter().any(|item| item.id == name) {
            continue;
        }
        let is_default = default_name.as_deref() == Some(name.as_str());
        devices.push(AudioOutputDevice {
            id: name.clone(),
            name,
            is_default,
        });
    }

    Ok(devices)
}

/// Builds and starts the CPAL capture streams for the requested sources.
///
/// This runs entirely on the audio thread because `cpal::Stream` is `!Send`;
/// the returned streams must be kept alive and dropped on the same thread.
pub(crate) fn build_capture_streams(
    capture_microphone: bool,
    capture_system: bool,
    audio_device_id: Option<&str>,
    system_audio_device_id: Option<&str>,
    capture_mode: &str,
    samples: Arc<Mutex<CaptureBuffers>>,
    pending: Option<Arc<Mutex<CaptureBuffers>>>,
    microphone_muted: Arc<AtomicBool>,
) -> Result<(Vec<cpal::Stream>, AudioSetup), String> {
    build_capture_streams_for_target(
        capture_microphone, capture_system, audio_device_id, system_audio_device_id,
        capture_mode, CaptureTarget::Raw(samples, pending), microphone_muted,
    )
}

pub(crate) fn build_recording_streams(
    capture_microphone: bool,
    capture_system: bool,
    audio_device_id: Option<&str>,
    system_audio_device_id: Option<&str>,
    capture_mode: &str,
    capture: Arc<Mutex<RecordingCapture>>,
    microphone_muted: Arc<AtomicBool>,
) -> Result<(Vec<cpal::Stream>, AudioSetup), String> {
    build_capture_streams_for_target(
        capture_microphone, capture_system, audio_device_id, system_audio_device_id,
        capture_mode, CaptureTarget::Recording(capture), microphone_muted,
    )
}

/// Recording streams stay paused through metadata and writer setup. Keep this
/// on their owning thread; microphone is first, loopback second, on ONE clock.
/// A play failure is fatal (unlike an optional loopback construction failure),
/// since the resolved capture mode has already been persisted at this point.
pub(crate) fn start_recording_streams(
    streams: &[cpal::Stream],
    capture: &Arc<Mutex<RecordingCapture>>,
) -> Result<Instant, String> {
    start_recording_sources(streams, capture, |stream| stream.play().map_err(|error| error.to_string()))
}

fn start_recording_sources<T>(
    streams: &[T],
    capture: &Arc<Mutex<RecordingCapture>>,
    mut play: impl FnMut(&T) -> Result<(), String>,
) -> Result<Instant, String> {
    let started_at = {
        let mut capture = capture.lock().map_err(|_| "Recording sample buffer lock poisoned".to_string())?;
        let started_at = Instant::now();
        capture.start(started_at)?;
        started_at
    };
    for stream in streams {
        {
            let capture = capture.lock().map_err(|_| "Recording sample buffer lock poisoned".to_string())?;
            if capture.stopped { return Err("Audio capture startup was cancelled".into()); }
            if let Some(error) = capture.error() { return Err(error.to_string()); }
        }
        if let Err(error) = play(stream) {
            if let Ok(mut capture) = capture.lock() { capture.stop(); }
            return Err(error);
        }
    }
    Ok(started_at)
}

fn build_capture_streams_for_target(
    capture_microphone: bool,
    capture_system: bool,
    audio_device_id: Option<&str>,
    system_audio_device_id: Option<&str>,
    capture_mode: &str,
    target: CaptureTarget,
    microphone_muted: Arc<AtomicBool>,
) -> Result<(Vec<cpal::Stream>, AudioSetup), String> {
    let host = audio_host();
    let mut streams = Vec::new();
    let mut setup = AudioSetup {
        resolved_capture_mode: capture_mode.to_string(),
        ..AudioSetup::default()
    };

    if capture_microphone {
        let (device, device_id, device_name) = resolve_audio_input_device(&host, audio_device_id)?;
        let config = device
            .default_input_config()
            .map_err(|error| error.to_string())?;
        let stream = build_capture_stream(
            &device,
            config,
            AudioSource::Microphone,
            target.clone(),
            microphone_muted.clone(),
        )?;
        if matches!(&target, CaptureTarget::Raw(..)) {
            stream.play().map_err(|error| error.to_string())?;
        }
        streams.push(stream);
        setup.resolved_audio_device_id = device_id;
        setup.audio_device_name = device_name;
    }

    if capture_system {
        match resolve_audio_output_device(&host, system_audio_device_id).and_then(
            |(device, device_id, device_name)| {
                let config = device
                    .default_output_config()
                    .map_err(|error| error.to_string())?;
                let stream = build_capture_stream(
                    &device,
                    config,
                    AudioSource::System,
                    target.clone(),
                    microphone_muted.clone(),
                )?;
                if matches!(&target, CaptureTarget::Raw(..)) {
                    stream.play().map_err(|error| error.to_string())?;
                }
                Ok((stream, device_id, device_name))
            },
        ) {
            Ok((stream, device_id, device_name)) => {
                streams.push(stream);
                setup.resolved_system_audio_device_id = device_id;
                setup.system_audio_device_name = device_name;
            }
            Err(error) if capture_microphone => {
                eprintln!("System audio capture could not start, continuing with microphone only: {error}");
                setup.resolved_capture_mode = "microphone".to_string();
                if let CaptureTarget::Recording(capture) = &target {
                    if let Ok(mut capture) = capture.lock() {
                        capture.mixer.capture_system = false;
                    }
                }
            }
            Err(error) => return Err(format!("System audio capture could not start: {error}")),
        }
    }

    if streams.is_empty() {
        return Err("No audio capture stream could be started".to_string());
    }

    Ok((streams, setup))
}

/// Opens a WAV destination before capture begins. Chunks are flushed during
/// recording so an interrupted session retains the audio written so far.
pub(crate) fn create_incremental_wav(
    path: &Path,
    sample_rate: u32,
) -> Result<IncrementalWavWriter> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    Ok(WavWriter::create(path, spec)?)
}

pub(crate) fn append_wav_chunk(writer: &mut IncrementalWavWriter, samples: &[f32]) -> Result<()> {
    for sample in samples {
        writer.write_sample(pcm16_sample(*sample))?;
    }
    writer.flush()?;
    Ok(())
}

// Rounding is intentional: PCM16 -> f32 -> PCM16 must be idempotent so
// persistence, Foundry WAV requests, and Azure chunks receive identical PCM.
pub(crate) fn pcm16_sample(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

pub(crate) fn pcm16_base64(samples: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let value = pcm16_sample(*sample);
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    general_purpose::STANDARD.encode(bytes)
}

/// Opens the recording, wires up a rodio `Sink`, and starts playback.
///
/// Runs on the playback thread because `OutputStream` is `!Send`; the stream is
/// returned alongside the `Sink` so the caller can keep it alive on that thread.
pub(crate) fn prepare_playback(
    recording_path: &str,
    offset_seconds: Option<f64>,
) -> Result<(Sink, OutputStream), String> {
    let file = fs::File::open(recording_path).map_err(|error| error.to_string())?;
    let source = Decoder::new(BufReader::new(file)).map_err(|error| error.to_string())?;
    let (stream, handle) = OutputStream::try_default().map_err(|error| error.to_string())?;
    let sink = Sink::try_new(&handle).map_err(|error| error.to_string())?;
    sink.append(source);
    if let Some(offset) = offset_seconds {
        if offset > 0.0 {
            let _ = sink.try_seek(Duration::from_secs_f64(offset));
        }
    }
    sink.play();
    Ok((sink, stream))
}

/// Stops a playback session and tears down its `OutputStream` on the owning thread.
pub(crate) fn stop_playback_session(session: PlaybackSession) {
    session.sink.stop();
    let _ = session.stop_tx.send(());
    let _ = session.thread.join();
}
