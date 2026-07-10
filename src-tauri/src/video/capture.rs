use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use crate::AppState;

const CAPTURE_STARTUP_TIMEOUT: Duration = Duration::from_millis(1_200);
const CAPTURE_STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_CAPTURE_DIAGNOSTIC_BYTES: usize = 32 * 1024;

pub(crate) struct ScreenCaptureProcess {
    child: Child,
    stderr_reader: Option<JoinHandle<String>>,
}

impl ScreenCaptureProcess {
    fn take_diagnostics(&mut self) -> String {
        self.stderr_reader
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for ScreenCaptureProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Bounds within the Windows virtual desktop. `windows-sys` exposes the
/// supported Win32 metrics without requiring a higher-level Windows framework.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScreenTarget {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

pub(crate) fn list_screen_targets() -> Vec<ScreenTarget> {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN,
        };

        let raw_width = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2);
        let raw_height = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2);
        let (width, height) = normalize_yuv420_dimensions(raw_width, raw_height).unwrap_or((2, 2));
        vec![ScreenTarget {
            id: "desktop".to_string(),
            name: "All displays".to_string(),
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width,
            height,
        }]
    }

    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

/// H.264 and H.265 with yuv420p require even frame dimensions. Cropping an
/// area down by one pixel is preferable to spawning FFmpeg with an invalid
/// encoder configuration that produces an empty video file.
pub(crate) fn normalize_yuv420_dimensions(width: i32, height: i32) -> Result<(i32, i32)> {
    if width < 2 || height < 2 {
        return Err(anyhow!(
            "Screen recording width and height must be at least two pixels."
        ));
    }

    let width = width - width.rem_euclid(2);
    let height = height - height.rem_euclid(2);
    Ok((width, height))
}

fn ffmpeg_path(app: &AppHandle) -> Result<PathBuf> {
    let configured_path = {
        let state = app.state::<AppState>();
        let store = state
            .store
            .lock()
            .map_err(|_| anyhow!("Store lock poisoned"))?;
        store.settings.ffmpeg_path.trim().to_string()
    };
    if !configured_path.is_empty() {
        let path = PathBuf::from(configured_path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "The configured FFmpeg executable is not available at {}. Select a valid executable in Settings or use the bundled runtime.",
            path.display()
        ));
    }

    if let Ok(resource_dir) = app.path().resource_dir() {
        for path in [
            resource_dir.join("ffmpeg.exe"),
            resource_dir.join("resources").join("ffmpeg.exe"),
        ] {
            if path.is_file() {
                return Ok(path);
            }
        }
    }

    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("ffmpeg.exe");
    if development.is_file() {
        return Ok(development);
    }

    Err(anyhow!(
        "FFmpeg is not available. Select an FFmpeg executable in Settings or package a license-compliant ffmpeg.exe in src-tauri/resources."
    ))
}

fn suppress_console_window(command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

fn trim_diagnostics(diagnostics: &str) -> String {
    const MAX_DIAGNOSTIC_LENGTH: usize = 4_096;

    let diagnostics = diagnostics.trim();
    if diagnostics.len() <= MAX_DIAGNOSTIC_LENGTH {
        return diagnostics.to_string();
    }

    let start = diagnostics
        .char_indices()
        .nth(
            diagnostics
                .chars()
                .count()
                .saturating_sub(MAX_DIAGNOSTIC_LENGTH),
        )
        .map(|(index, _)| index)
        .unwrap_or(0);
    format!("…{}", &diagnostics[start..])
}

fn ffmpeg_failure(operation: &str, status: ExitStatus, diagnostics: &str) -> anyhow::Error {
    let diagnostics = trim_diagnostics(diagnostics);
    if diagnostics.is_empty() {
        anyhow!("{operation} with status {status}")
    } else {
        anyhow!("{operation} with status {status}: {diagnostics}")
    }
}

fn spawn_stderr_reader(mut stderr: ChildStderr) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut tail = Vec::with_capacity(MAX_CAPTURE_DIAGNOSTIC_BYTES);
        let mut buffer = [0_u8; 4_096];
        // Drain stderr continuously so a long recording never blocks FFmpeg on a
        // full pipe, keeping only a bounded tail for diagnostics.
        loop {
            let count = match stderr.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            let overflow = tail
                .len()
                .saturating_add(count)
                .saturating_sub(MAX_CAPTURE_DIAGNOSTIC_BYTES);
            if overflow > 0 {
                tail.drain(..overflow);
            }
            tail.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8_lossy(&tail).into_owned()
    })
}

fn verify_capture_started(capture: &mut ScreenCaptureProcess) -> Result<()> {
    // A misconfigured encoder (for example an odd yuv420p frame size) makes
    // FFmpeg exit within a few hundred milliseconds. Watch for that early exit;
    // if the process is still running after a short window, capture has started.
    let deadline = Instant::now() + CAPTURE_STARTUP_TIMEOUT;
    loop {
        if let Some(status) = capture
            .child
            .try_wait()
            .context("Could not check FFmpeg screen capture status")?
        {
            let diagnostics = capture.take_diagnostics();
            return Err(ffmpeg_failure(
                "FFmpeg exited before screen capture started",
                status,
                &diagnostics,
            ));
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(CAPTURE_STARTUP_POLL_INTERVAL);
    }
}

pub(crate) fn start_capture(
    app: &AppHandle,
    target: &ScreenTarget,
    codec: &str,
    output: &Path,
) -> Result<ScreenCaptureProcess> {
    let ffmpeg = ffmpeg_path(app)?;
    let (width, height) = normalize_yuv420_dimensions(target.width, target.height)?;
    let video_codec = if codec == "h265" {
        "libx265"
    } else {
        "libx264"
    };
    let dimensions = format!("{width}x{height}");
    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "warning",
            "-y",
            "-f",
            "gdigrab",
        ])
        .args([
            "-draw_mouse",
            "1",
            "-framerate",
            "15",
            "-offset_x",
            &target.x.to_string(),
            "-offset_y",
            &target.y.to_string(),
            "-video_size",
            &dimensions,
            "-i",
            "desktop",
            "-c:v",
            video_codec,
            "-preset",
            "veryfast",
            "-crf",
            "28",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+frag_keyframe+empty_moov+default_base_moof",
        ])
        .arg(output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    suppress_console_window(&mut command);
    let mut child = command
        .spawn()
        .context("Could not start FFmpeg screen capture")?;
    let stderr_reader = child.stderr.take().map(spawn_stderr_reader);
    let mut capture = ScreenCaptureProcess {
        child,
        stderr_reader,
    };
    // On failure `capture` is dropped, which terminates the FFmpeg child.
    verify_capture_started(&mut capture)?;
    Ok(capture)
}

pub(crate) fn stop_capture(capture: &mut ScreenCaptureProcess) -> Result<()> {
    let input_error = capture.child.stdin.take().and_then(|mut stdin| {
        stdin
            .write_all(b"q\n")
            .and_then(|_| stdin.flush())
            .err()
            .filter(|error| error.kind() != ErrorKind::BrokenPipe)
    });
    let status = capture
        .child
        .wait()
        .context("Could not wait for FFmpeg to stop")?;
    let diagnostics = capture.take_diagnostics();
    if let Some(error) = input_error {
        Err(anyhow!("Could not send FFmpeg its stop command: {error}"))
    } else if status.success() {
        Ok(())
    } else {
        Err(ffmpeg_failure("FFmpeg stopped", status, &diagnostics))
    }
}

fn run_ffmpeg_command(command: &mut Command, operation: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("Could not start FFmpeg to {operation}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ffmpeg_failure(
            &format!("FFmpeg could not {operation}"),
            output.status,
            &String::from_utf8_lossy(&output.stderr),
        ))
    }
}

pub(crate) fn validate_recording(app: &AppHandle, recording: &Path) -> Result<()> {
    let ffmpeg = ffmpeg_path(app)?;
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(recording)
        .args(["-map", "0:v:0", "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    suppress_console_window(&mut command);
    run_ffmpeg_command(&mut command, "validate the screen recording")
}

pub(crate) fn mux_audio(
    app: &AppHandle,
    video: &Path,
    audio: &Path,
    destination: &Path,
) -> Result<()> {
    let ffmpeg = ffmpeg_path(app)?;
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "warning", "-y", "-i"])
        .arg(video)
        .arg("-i")
        .arg(audio)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-shortest",
        ])
        .arg(destination)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    suppress_console_window(&mut command);
    run_ffmpeg_command(&mut command, "add audio to the screen recording")
}

pub(crate) fn extract_mp3(app: &AppHandle, source: &Path, destination: &Path) -> Result<()> {
    let ffmpeg = ffmpeg_path(app)?;
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "warning", "-y", "-i"])
        .arg(source)
        .args(["-vn", "-codec:a", "libmp3lame", "-q:a", "3"])
        .arg(destination)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    suppress_console_window(&mut command);
    run_ffmpeg_command(&mut command, "extract MP3 audio from the video")
}
