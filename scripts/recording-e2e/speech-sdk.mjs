// TEST ONLY: Vite resolves the SDK import here only in recording-e2e.mjs.
// Re-exported SDK configuration/audio objects are real. Only recognition/service is fake.
import SDK from 'recording-e2e-real-sdk';
// Explicit exports retain the installed CJS objects through Vite interop;
// export-star cannot preserve them. None of these constructors are patched.
export const { SpeechConfig, PropertyId, AudioStreamFormat, AudioInputStream,
  AudioConfig, ResultReason, CancellationReason } = SDK;

export class SpeechRecognizer {
  constructor(config, audio) {
    this.config = config;
    this.audio = audio;
    this.state = window.__recordingE2E;
    this.result = { reads: [], bytes: 0, eof: false, closed: 0, forcedStops: 0 };
    this.state.speech.push(this.result);
  }

  startContinuousRecognitionAsync(success, failure) {
    void (async () => {
      // Exercise the installed AudioConfig -> PushAudioInputStream -> chunked
      // reader, including the SDK's real buffering and natural EOF semantics.
      const format = await this.audio.format;
      this.result.format = {
        samplesPerSec: format.samplesPerSec,
        bitsPerSample: format.bitsPerSample,
        channels: format.channels,
      };
      this.result.language = this.config.speechRecognitionLanguage;
      this.reader = await this.audio.attach('offline-recording-e2e');
      this.state.log.push('recognizer.started');
      success();
      let early = false;
      for (;;) {
        const chunk = await this.reader.read();
        if (chunk.buffer?.byteLength) {
          const bytes = new Uint8Array(chunk.buffer);
          this.result.reads.push(btoa(String.fromCharCode(...bytes)));
          this.result.bytes += bytes.length;
          if (!early) {
            early = true;
            this.recognizing?.(this, { result: { text: 'SYNTHETIC partial — offline recognizer' } });
            this.final('SYNTHETIC early — offline recognizer, not actual speech', 0.125);
          }
        }
        if (chunk.isEnd) break;
      }
      this.result.eof = true;
      this.state.log.push('speech.eof');
      this.final('SYNTHETIC final tail — offline recognizer, not actual speech', this.result.bytes / 32000 - 0.1);
      this.canceled?.(this, { reason: SDK.CancellationReason.EndOfStream });
      this.sessionStopped?.(this, {});
      this.state.log.push('speech.sessionStopped');
    })().catch((error) => {
      this.state.failures.push(String(error.stack || error));
      failure?.(String(error));
    });
  }

  final(text, seconds) {
    this.recognized?.(this, {
      result: { reason: SDK.ResultReason.RecognizedSpeech, text, offset: Math.round(seconds * 10_000_000) },
    });
  }

  stopContinuousRecognitionAsync(success) {
    this.result.forcedStops++;
    this.state.failures.push('Premature forced recognizer stop');
    success?.();
  }

  close(success, failure) {
    this.result.closed++;
    this.state.log.push('recognizer.close');
    // Do not call AudioConfig.close: the real application owns input lifetime.
    Promise.resolve(this.reader?.detach()).then(() => success?.(), failure);
  }
}