use crate::{AudioInputDevice, AudioOutputDevice, PlaybackSession};
use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{SampleFormat, WavSpec, WavWriter};
use rodio::{Decoder, OutputStream, Sink};
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

fn resample_to_16khz(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 16_000 {
        return samples.to_vec();
    }
    let ratio = source_rate as f64 / 16_000.0;
    let out_len = (samples.len() as f64 / ratio).floor() as usize;
    (0..out_len)
        .map(|idx| {
            let source_idx = (idx as f64 * ratio).floor() as usize;
            samples.get(source_idx).copied().unwrap_or_default()
        })
        .collect()
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

fn append_capture_samples(
    buffers: &Arc<Mutex<CaptureBuffers>>,
    source: AudioSource,
    samples: &[f32],
) {
    if let Ok(mut buffers) = buffers.lock() {
        match source {
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
    samples: Arc<Mutex<CaptureBuffers>>,
    pending: Option<Arc<Mutex<CaptureBuffers>>>,
) -> Result<cpal::Stream, String> {
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let error_callback = |error| eprintln!("Audio stream error: {error}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                let mono = downmix_to_mono(data, channels);
                let samples_16k = resample_to_16khz(&mono, sample_rate);
                append_capture_samples(&samples, source, &samples_16k);
                if let Some(pending) = pending.as_ref() {
                    append_capture_samples(pending, source, &samples_16k);
                }
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _| {
                let converted = convert_samples_i16(data);
                let mono = downmix_to_mono(&converted, channels);
                let samples_16k = resample_to_16khz(&mono, sample_rate);
                append_capture_samples(&samples, source, &samples_16k);
                if let Some(pending) = pending.as_ref() {
                    append_capture_samples(pending, source, &samples_16k);
                }
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _| {
                let converted = convert_samples_u16(data);
                let mono = downmix_to_mono(&converted, channels);
                let samples_16k = resample_to_16khz(&mono, sample_rate);
                append_capture_samples(&samples, source, &samples_16k);
                if let Some(pending) = pending.as_ref() {
                    append_capture_samples(pending, source, &samples_16k);
                }
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
            samples.clone(),
            pending.clone(),
        )?;
        stream.play().map_err(|error| error.to_string())?;
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
                    samples.clone(),
                    pending.clone(),
                )?;
                stream.play().map_err(|error| error.to_string())?;
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
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        writer.write_sample(value)?;
    }
    writer.flush()?;
    Ok(())
}

pub(crate) fn pcm16_base64(samples: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
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
