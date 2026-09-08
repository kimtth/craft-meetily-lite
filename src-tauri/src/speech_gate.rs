use std::collections::VecDeque;
use wavekat_vad::{
    backends::webrtc::{WebRtcVad, WebRtcVadMode},
    VoiceActivityDetector,
};

const SAMPLE_RATE: u32 = 16_000;
const FRAME_DURATION_MS: u32 = 20;
const FRAME_SAMPLES: usize = (SAMPLE_RATE as usize * FRAME_DURATION_MS as usize) / 1_000;
const MIN_VOICED_FRAMES: usize = 6;
const MIN_CONSECUTIVE_VOICED_FRAMES: usize = 3;
const PRE_ROLL_FRAMES: usize = 15;
const TRAILING_SILENCE_FRAMES: usize = 75;
const MAX_UTTERANCE_FRAMES: usize = 750;
const FORCE_SPLIT_OVERLAP_FRAMES: usize = 15;
const MIN_STANDALONE_VOICED_FRAMES: usize = 100;
const LOW_SPEECH_WAIT_SAMPLES: u64 = SAMPLE_RATE as u64 * 2;
const MAX_BUFFERED_LOW_SPEECH_SAMPLES: usize = SAMPLE_RATE as usize * 5;

#[derive(Debug)]
pub(crate) struct SpeechUtterance {
    pub(crate) offset_samples: u64,
    pub(crate) samples: Vec<f32>,
    pub(crate) voiced_frames: usize,
}

#[derive(Debug)]
struct PendingLowSpeech {
    utterance: SpeechUtterance,
    deadline_samples: u64,
}

#[derive(Debug, Default)]
pub(crate) struct LowSpeechUtteranceBuffer {
    pending: Option<PendingLowSpeech>,
}

impl LowSpeechUtteranceBuffer {
    pub(crate) fn push(
        &mut self,
        mut utterance: SpeechUtterance,
        completed_at_samples: u64,
    ) -> Vec<SpeechUtterance> {
        let mut emitted = Vec::new();
        if let Some(mut pending) = self.pending.take() {
            if utterance.offset_samples <= pending.deadline_samples {
                pending.utterance.samples.append(&mut utterance.samples);
                pending.utterance.voiced_frames += utterance.voiced_frames;
                emitted.push(pending.utterance);
                return emitted;
            }
            emitted.push(pending.utterance);
        }

        if utterance.voiced_frames < MIN_STANDALONE_VOICED_FRAMES
            && utterance.samples.len() <= MAX_BUFFERED_LOW_SPEECH_SAMPLES
        {
            self.pending = Some(PendingLowSpeech {
                utterance,
                deadline_samples: completed_at_samples + LOW_SPEECH_WAIT_SAMPLES,
            });
        } else {
            emitted.push(utterance);
        }
        emitted
    }

    pub(crate) fn advance(
        &mut self,
        processed_samples: u64,
        speech_in_progress: bool,
    ) -> Vec<SpeechUtterance> {
        let should_emit = self.pending.as_ref().is_some_and(|pending| {
            processed_samples >= pending.deadline_samples && !speech_in_progress
        });
        if should_emit {
            self.flush()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn flush(&mut self) -> Vec<SpeechUtterance> {
        self.pending
            .take()
            .map(|pending| vec![pending.utterance])
            .unwrap_or_default()
    }
}

#[derive(Debug)]
struct BufferedFrame {
    offset_samples: u64,
    samples: Vec<f32>,
    voiced: bool,
}

#[derive(Debug)]
struct ActiveUtterance {
    offset_samples: u64,
    samples: Vec<f32>,
    voiced_frames_since_emit: usize,
    trailing_silence_frames: usize,
}

#[derive(Debug, Default)]
struct UtteranceSegmenter {
    pre_roll: VecDeque<BufferedFrame>,
    candidate_frames: VecDeque<BufferedFrame>,
    active: Option<ActiveUtterance>,
    processed_samples: u64,
}

impl UtteranceSegmenter {
    fn new() -> Self {
        Self::default()
    }

    fn process_frame(&mut self, samples: &[f32], voiced: bool) -> Option<SpeechUtterance> {
        debug_assert_eq!(samples.len(), FRAME_SAMPLES);
        let frame_offset = self.processed_samples;
        self.processed_samples += samples.len() as u64;
        let frame = BufferedFrame {
            offset_samples: frame_offset,
            samples: samples.to_vec(),
            voiced,
        };

        if self.active.is_some() {
            self.push_pre_roll(frame);
            let active = self.active.as_mut().expect("active utterance checked");
            active.samples.extend_from_slice(samples);
            if voiced {
                active.voiced_frames_since_emit += 1;
                active.trailing_silence_frames = 0;
            } else {
                active.trailing_silence_frames += 1;
            }
            if active.samples.len() >= MAX_UTTERANCE_FRAMES * FRAME_SAMPLES {
                return self.force_split();
            }
            if active.trailing_silence_frames >= TRAILING_SILENCE_FRAMES {
                return self.finish_active();
            }
            return None;
        }

        if self.candidate_frames.is_empty() && !voiced {
            self.push_pre_roll(frame);
            return None;
        }
        self.candidate_frames.push_back(frame);
        let voiced_frames = self
            .candidate_frames
            .iter()
            .map(|frame| frame.voiced)
            .collect::<Vec<_>>();
        if has_sufficient_speech_from_frames(&voiced_frames) {
            let offset_samples = self
                .pre_roll
                .front()
                .or_else(|| self.candidate_frames.front())
                .map(|frame| frame.offset_samples)
                .unwrap_or(frame_offset);
            let samples = self
                .pre_roll
                .iter()
                .chain(self.candidate_frames.iter())
                .flat_map(|frame| frame.samples.iter().copied())
                .collect();
            self.active = Some(ActiveUtterance {
                offset_samples,
                samples,
                voiced_frames_since_emit: voiced_frames.iter().filter(|value| **value).count(),
                trailing_silence_frames: 0,
            });
            self.pre_roll.clear();
            self.candidate_frames.clear();
        } else if self.candidate_frames.len() >= PRE_ROLL_FRAMES {
            while let Some(candidate) = self.candidate_frames.pop_front() {
                self.push_pre_roll(candidate);
            }
        }
        None
    }

    fn push_pre_roll(&mut self, frame: BufferedFrame) {
        self.pre_roll.push_back(frame);
        if self.pre_roll.len() > PRE_ROLL_FRAMES {
            self.pre_roll.pop_front();
        }
    }

    fn force_split(&mut self) -> Option<SpeechUtterance> {
        let active = self.active.take()?;
        let utterance = SpeechUtterance {
            offset_samples: active.offset_samples,
            samples: active.samples.clone(),
            voiced_frames: active.voiced_frames_since_emit,
        };
        let overlap_samples = FORCE_SPLIT_OVERLAP_FRAMES * FRAME_SAMPLES;
        let overlap_start = active.samples.len().saturating_sub(overlap_samples);
        self.active = Some(ActiveUtterance {
            offset_samples: active.offset_samples + overlap_start as u64,
            samples: active.samples[overlap_start..].to_vec(),
            voiced_frames_since_emit: 0,
            trailing_silence_frames: 0,
        });
        Some(utterance)
    }

    fn finish_active(&mut self) -> Option<SpeechUtterance> {
        let mut active = self.active.take()?;
        let trailing_samples = active.trailing_silence_frames * FRAME_SAMPLES;
        active
            .samples
            .truncate(active.samples.len().saturating_sub(trailing_samples));
        if active.voiced_frames_since_emit == 0 || active.samples.is_empty() {
            return None;
        }
        Some(SpeechUtterance {
            offset_samples: active.offset_samples,
            samples: active.samples,
            voiced_frames: active.voiced_frames_since_emit,
        })
    }

    fn speech_in_progress(&self) -> bool {
        self.active.is_some() || !self.candidate_frames.is_empty()
    }

    fn flush(&mut self) -> Option<SpeechUtterance> {
        self.finish_active()
    }

    fn append_partial_to_active(&mut self, samples: &[f32], voiced: bool) {
        if let Some(active) = self.active.as_mut() {
            active.samples.extend_from_slice(samples);
            if voiced {
                active.voiced_frames_since_emit += 1;
            }
        }
        self.processed_samples += samples.len() as u64;
    }
}

pub(crate) struct StreamingSpeechSegmenter {
    vad: WebRtcVad,
    segmenter: UtteranceSegmenter,
    low_speech: LowSpeechUtteranceBuffer,
    pending_samples: Vec<f32>,
}

pub(crate) fn has_sufficient_speech(samples: &[f32]) -> Result<bool, String> {
    let mut vad =
        WebRtcVad::with_frame_duration(SAMPLE_RATE, WebRtcVadMode::Aggressive, FRAME_DURATION_MS)
            .map_err(|error| format!("Failed to initialize WebRTC VAD: {error}"))?;
    let mut voiced_frames = Vec::with_capacity(samples.len() / FRAME_SAMPLES);
    for frame in samples.chunks_exact(FRAME_SAMPLES) {
        let pcm = frame
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect::<Vec<_>>();
        voiced_frames.push(
            vad.process(&pcm, SAMPLE_RATE)
                .map_err(|error| format!("WebRTC VAD failed: {error}"))?
                >= 0.5,
        );
    }
    Ok(has_sufficient_speech_from_frames(&voiced_frames))
}

impl StreamingSpeechSegmenter {
    pub(crate) fn new() -> Result<Self, String> {
        let vad = WebRtcVad::with_frame_duration(
            SAMPLE_RATE,
            WebRtcVadMode::Aggressive,
            FRAME_DURATION_MS,
        )
        .map_err(|error| format!("Failed to initialize WebRTC VAD: {error}"))?;
        Ok(Self {
            vad,
            segmenter: UtteranceSegmenter::new(),
            low_speech: LowSpeechUtteranceBuffer::default(),
            pending_samples: Vec::new(),
        })
    }

    pub(crate) fn push_samples(&mut self, samples: &[f32]) -> Result<Vec<SpeechUtterance>, String> {
        self.pending_samples.extend_from_slice(samples);
        let complete_samples = (self.pending_samples.len() / FRAME_SAMPLES) * FRAME_SAMPLES;
        let mut utterances = Vec::new();

        for frame in self.pending_samples[..complete_samples].chunks_exact(FRAME_SAMPLES) {
            let pcm = frame
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .collect::<Vec<_>>();
            let voiced = self
                .vad
                .process(&pcm, SAMPLE_RATE)
                .map_err(|error| format!("WebRTC VAD failed: {error}"))?
                >= 0.5;
            if let Some(utterance) = self.segmenter.process_frame(frame, voiced) {
                utterances.extend(
                    self.low_speech
                        .push(utterance, self.segmenter.processed_samples),
                );
            } else {
                utterances.extend(self.low_speech.advance(
                    self.segmenter.processed_samples,
                    self.segmenter.speech_in_progress(),
                ));
            }
        }
        self.pending_samples.drain(..complete_samples);
        Ok(utterances)
    }

    pub(crate) fn flush(&mut self) -> Result<Vec<SpeechUtterance>, String> {
        if !self.pending_samples.is_empty() {
            let mut padded = self.pending_samples.clone();
            padded.resize(FRAME_SAMPLES, 0.0);
            let pcm = padded
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .collect::<Vec<_>>();
            let voiced = self
                .vad
                .process(&pcm, SAMPLE_RATE)
                .map_err(|error| format!("WebRTC VAD failed during flush: {error}"))?
                >= 0.5;
            self.segmenter
                .append_partial_to_active(&self.pending_samples, voiced);
            self.pending_samples.clear();
        }
        let mut utterances = Vec::new();
        if let Some(utterance) = self.segmenter.flush() {
            utterances.extend(
                self.low_speech
                    .push(utterance, self.segmenter.processed_samples),
            );
        }
        utterances.extend(self.low_speech.flush());
        Ok(utterances)
    }
}

fn has_sufficient_speech_from_frames(frames: &[bool]) -> bool {
    let voiced_count = frames.iter().filter(|voiced| **voiced).count();
    if voiced_count < MIN_VOICED_FRAMES {
        return false;
    }

    let mut consecutive = 0;
    for voiced in frames {
        consecutive = if *voiced { consecutive + 1 } else { 0 };
        if consecutive >= MIN_CONSECUTIVE_VOICED_FRAMES {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        has_sufficient_speech, has_sufficient_speech_from_frames, LowSpeechUtteranceBuffer,
        SpeechUtterance, StreamingSpeechSegmenter, UtteranceSegmenter, FORCE_SPLIT_OVERLAP_FRAMES,
        FRAME_SAMPLES, LOW_SPEECH_WAIT_SAMPLES, MAX_BUFFERED_LOW_SPEECH_SAMPLES,
        MIN_STANDALONE_VOICED_FRAMES,
    };

    fn frame(value: f32) -> Vec<f32> {
        vec![value; FRAME_SAMPLES]
    }

    fn utterance(offset_samples: u64, voiced_frames: usize) -> SpeechUtterance {
        SpeechUtterance {
            offset_samples,
            samples: vec![0.5; voiced_frames * FRAME_SAMPLES],
            voiced_frames,
        }
    }

    #[test]
    fn rejects_silence_and_isolated_voice_frames() {
        assert!(!has_sufficient_speech_from_frames(&[false; 250]));

        let mut isolated = [false; 250];
        for index in [10, 40, 80, 120, 160, 200] {
            isolated[index] = true;
        }
        assert!(!has_sufficient_speech_from_frames(&isolated));
    }

    #[test]
    fn accepts_sustained_speech() {
        let mut frames = [false; 250];
        frames[40..55].fill(true);
        assert!(has_sufficient_speech_from_frames(&frames));
    }

    #[test]
    fn low_speech_utterance_waits_then_emits_alone() {
        let mut buffer = LowSpeechUtteranceBuffer::default();
        let completed_at = 10_000;
        assert!(buffer
            .push(utterance(0, MIN_STANDALONE_VOICED_FRAMES - 1), completed_at)
            .is_empty());
        assert!(buffer
            .advance(completed_at + LOW_SPEECH_WAIT_SAMPLES - 1, false)
            .is_empty());

        let emitted = buffer.advance(completed_at + LOW_SPEECH_WAIT_SAMPLES, false);
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].offset_samples, 0);
    }

    #[test]
    fn low_speech_utterance_merges_with_nearby_following_speech() {
        let mut buffer = LowSpeechUtteranceBuffer::default();
        let completed_at = 10_000;
        let first = utterance(1_000, MIN_STANDALONE_VOICED_FRAMES - 1);
        let first_len = first.samples.len();
        assert!(buffer.push(first, completed_at).is_empty());

        let second_offset = completed_at + LOW_SPEECH_WAIT_SAMPLES - 1;
        let emitted = buffer.push(
            utterance(second_offset, MIN_STANDALONE_VOICED_FRAMES),
            second_offset + 20_000,
        );
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].offset_samples, 1_000);
        assert_eq!(
            emitted[0].samples.len(),
            first_len + MIN_STANDALONE_VOICED_FRAMES * FRAME_SAMPLES
        );
        assert_eq!(
            emitted[0].voiced_frames,
            MIN_STANDALONE_VOICED_FRAMES * 2 - 1
        );
    }

    #[test]
    fn active_following_speech_prevents_timeout_until_it_finishes() {
        let mut buffer = LowSpeechUtteranceBuffer::default();
        let completed_at = 10_000;
        assert!(buffer
            .push(utterance(0, MIN_STANDALONE_VOICED_FRAMES - 1), completed_at)
            .is_empty());
        assert!(buffer
            .advance(completed_at + LOW_SPEECH_WAIT_SAMPLES, true)
            .is_empty());
        assert_eq!(buffer.flush().len(), 1);
    }

    #[test]
    fn long_low_speech_utterance_is_not_buffered() {
        let mut buffer = LowSpeechUtteranceBuffer::default();
        let mut long = utterance(0, MIN_STANDALONE_VOICED_FRAMES - 1);
        long.samples
            .resize(MAX_BUFFERED_LOW_SPEECH_SAMPLES + 1, 0.0);

        assert_eq!(buffer.push(long, 10_000).len(), 1);
        assert!(buffer.flush().is_empty());
    }

    #[test]
    fn actual_webrtc_detector_rejects_digital_silence() {
        let mut segmenter = StreamingSpeechSegmenter::new().unwrap();
        assert!(segmenter.push_samples(&[0.0; 100]).unwrap().is_empty());
        assert!(segmenter
            .push_samples(&vec![0.0; 15_900])
            .unwrap()
            .is_empty());
        assert!(segmenter.flush().unwrap().is_empty());
        assert!(!has_sufficient_speech(&vec![0.0; 16_000]).unwrap());
    }

    #[test]
    fn utterance_keeps_pre_roll_and_ends_after_trailing_silence() {
        let mut segmenter = UtteranceSegmenter::new();
        for _ in 0..15 {
            assert!(segmenter.process_frame(&frame(0.0), false).is_none());
        }
        for _ in 0..6 {
            assert!(segmenter.process_frame(&frame(0.5), true).is_none());
        }

        for _ in 0..35 {
            assert!(segmenter.process_frame(&frame(0.0), false).is_none());
        }

        let mut emitted = None;
        for _ in 35..75 {
            emitted = segmenter.process_frame(&frame(0.0), false).or(emitted);
        }

        let utterance =
            emitted.expect("1.5 seconds of trailing silence should finish the utterance");
        assert_eq!(utterance.offset_samples, 0);
        assert_eq!(utterance.samples.len(), 21 * FRAME_SAMPLES);
    }

    #[test]
    fn isolated_noise_never_starts_or_flushes_an_utterance() {
        let mut segmenter = UtteranceSegmenter::new();
        for index in 0..250 {
            let voiced = matches!(index, 10 | 40 | 80 | 120 | 160 | 200);
            assert!(segmenter
                .process_frame(&frame(if voiced { 0.8 } else { 0.0 }), voiced)
                .is_none());
        }
        assert!(segmenter.flush().is_none());
    }

    #[test]
    fn long_speech_force_splits_with_overlap_and_monotonic_offsets() {
        let mut segmenter = UtteranceSegmenter::new();
        let mut first = None;
        for _ in 0..750 {
            first = segmenter.process_frame(&frame(0.5), true).or(first);
        }
        let first = first.expect("15 seconds should force a split");
        assert_eq!(first.offset_samples, 0);
        assert_eq!(first.samples.len(), 750 * FRAME_SAMPLES);

        for _ in 0..10 {
            assert!(segmenter.process_frame(&frame(0.5), true).is_none());
        }
        let second = segmenter
            .flush()
            .expect("new speech after the split should flush");
        assert_eq!(second.offset_samples, 735 * FRAME_SAMPLES as u64);
        assert_eq!(second.samples.len(), 25 * FRAME_SAMPLES);
        assert!(second.offset_samples > first.offset_samples);
    }

    #[test]
    fn flush_emits_active_speech_but_not_force_split_overlap_alone() {
        let mut active = UtteranceSegmenter::new();
        for _ in 0..10 {
            assert!(active.process_frame(&frame(0.5), true).is_none());
        }
        active.append_partial_to_active(&[0.5; 100], true);
        assert_eq!(
            active
                .flush()
                .expect("active speech should flush")
                .samples
                .len(),
            10 * FRAME_SAMPLES + 100
        );

        let mut split = UtteranceSegmenter::new();
        for _ in 0..750 {
            let _ = split.process_frame(&frame(0.5), true);
        }
        assert!(split.flush().is_none());

        let mut split_with_partial = UtteranceSegmenter::new();
        for _ in 0..750 {
            let _ = split_with_partial.process_frame(&frame(0.5), true);
        }
        split_with_partial.append_partial_to_active(&[0.5; 100], true);
        assert_eq!(
            split_with_partial
                .flush()
                .expect("unique partial speech should flush")
                .samples
                .len(),
            FORCE_SPLIT_OVERLAP_FRAMES * FRAME_SAMPLES + 100
        );
    }
}
