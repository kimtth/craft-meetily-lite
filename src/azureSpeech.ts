import { getAzureCliAccessToken } from './nativeClient';
import { normalizeAzureLanguage } from './audio/recordingOptions';
import { PcmTimeline } from './audio/pcmTimeline';

export type AzureSpeechOptions = {
  endpoint: string;
  tenantId?: string;
  subscriptionId?: string;
  language: string;
  onFinalText: (text: string, offsetSeconds: number) => void | Promise<void>;
  onPartialText?: (text: string) => void;
  onError?: (message: string) => void;
};

export type AzureSpeechSession = {
  /** With an offset, final results use absolute saved-recording time. Without
   * offsets, results remain relative to this session for legacy callers. */
  pushPcmBase64: (pcmBase64: string, offsetSeconds?: number) => void;
  stop: () => Promise<void>;
};

// A stalled service/store must not block a language change forever. Expiry is
// a failed/incomplete transcription, never a successful flush of queued PCM.
const DRAIN_TIMEOUT_MS = 120_000;
const START_TIMEOUT_MS = 30_000;
const DISPOSE_TIMEOUT_MS = 5_000;

async function withDeadline(operation: Promise<void>, milliseconds: number, message: string): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      operation,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function decodeBase64(base64: string): ArrayBuffer {
  const binary = window.atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes.buffer;
}

function normalizeSpeechEndpoint(endpoint: string): URL {
  const trimmed = endpoint.trim();
  if (!trimmed) {
    throw new Error('Enter the custom domain endpoint for the Azure Speech resource.');
  }

  const url = new URL(trimmed);
  const isRegionalCognitiveEndpoint = url.hostname.endsWith('.api.cognitive.microsoft.com');
  const customDomainSuffix = '.cognitiveservices.azure.com';
  const resourceName = url.hostname.endsWith(customDomainSuffix)
    ? url.hostname.slice(0, -customDomainSuffix.length)
    : '';
  const isCustomDomainEndpoint = /^[a-z0-9-]+$/i.test(resourceName)
    && url.protocol === 'https:'
    && !url.username
    && !url.password
    && (!url.port || url.port === '443')
    && url.pathname === '/'
    && !url.search
    && !url.hash;

  if (isCustomDomainEndpoint) {
    return url;
  }

  if (isRegionalCognitiveEndpoint || /^[a-z0-9-]+$/i.test(trimmed)) {
    throw new Error('Azure CLI sign-in uses Microsoft Entra authentication. Microsoft documentation requires a Speech resource custom domain endpoint such as https://your-custom-name.cognitiveservices.azure.com/. Regional endpoints such as eastus or https://eastus.api.cognitive.microsoft.com/ are not supported for this auth mode.');
  }

  throw new Error('Use the custom domain endpoint for the Azure Speech resource, for example https://your-custom-name.cognitiveservices.azure.com/.');
}

export async function startAzureSpeechSession(options: AzureSpeechOptions): Promise<AzureSpeechSession> {
  // A recognizer owns its callbacks, credentials, and language for its whole
  // lifetime; later caller state changes must not rebind an in-flight session.
  options = { ...options, language: normalizeAzureLanguage(options.language) };
  const SpeechSDK = await import('microsoft-cognitiveservices-speech-sdk');
  let cachedToken: Awaited<ReturnType<typeof getAzureCliAccessToken>> | null = null;
  const speechConfig = SpeechSDK.SpeechConfig.fromEndpoint(normalizeSpeechEndpoint(options.endpoint), {
    getToken: async () => {
      if (cachedToken && cachedToken.expiresOnTimestamp > Date.now() + 60_000) {
        return cachedToken;
      }
      cachedToken = await getAzureCliAccessToken(options.tenantId, options.subscriptionId);
      return cachedToken;
    },
  });
  speechConfig.speechRecognitionLanguage = options.language;
  speechConfig.setProperty(SpeechSDK.PropertyId.Speech_SegmentationStrategy, 'Semantic');

  const audioFormat = SpeechSDK.AudioStreamFormat.getWaveFormatPCM(16000, 16, 1);
  const pushStream = SpeechSDK.AudioInputStream.createPushStream(audioFormat);
  const audioConfig = SpeechSDK.AudioConfig.fromStreamInput(pushStream);
  const recognizer = new SpeechSDK.SpeechRecognizer(speechConfig, audioConfig);
  const timeline = new PcmTimeline();
  const pendingFinals = new Set<Promise<void>>();
  let closed = false;
  let inputClosed = false;
  let sessionEnded = false;
  let stopping = false;
  let failure: Error | undefined;
  let stopPromise: Promise<void> | undefined;
  let resolveSessionEnd!: () => void;
  const sessionEnd = new Promise<void>((resolve) => { resolveSessionEnd = resolve; });

  const recordFailure = (error: unknown) => {
    if (!failure) {
      failure = new Error(`Azure Speech transcription may be incomplete: ${error instanceof Error ? error.message : String(error)}`);
      // A consumer's error callback must not prevent cleanup or escape into
      // the SDK's receive loop (which can otherwise stop delivering events).
      try { options.onError?.(failure.message); } catch { /* best-effort notification */ }
    }
  };

  const finishSession = () => {
    sessionEnded = true;
    resolveSessionEnd();
  };

  const closeInput = () => {
    if (inputClosed) return;
    inputClosed = true;
    pushStream.close();
  };

  const stop = (): Promise<void> => {
    if (stopPromise) return stopPromise;
    stopping = true;
    // Assign the shared promise BEFORE closing input: mocks and SDK callbacks
    // may signal cancellation/sessionStopped synchronously from close().
    stopPromise = Promise.resolve().then(async () => {
      try {
        try {
          closeInput();
        } catch (error) {
          recordFailure(error);
          finishSession();
        }
        await withDeadline((async () => {
          // SDK Stream.close queues EOF after buffered PCM. In contrast,
          // stopContinuousRecognitionAsync stops the sender without draining.
          await sessionEnd;
          // All recognized callbacks precede sessionStopped in the SDK receive
          // loop; their async persistence may still be running at that point.
          await Promise.all([...pendingFinals]);
        })(), DRAIN_TIMEOUT_MS, 'Timed out waiting for end-of-input recognition and final text persistence.');
      } catch (error) {
        recordFailure(error);
      } finally {
        closed = true;
        try {
          // close() itself is asynchronous in Speech SDK. On the normal path
          // it is called only AFTER natural completion and final persistence.
          await withDeadline(new Promise<void>((resolve, reject) => {
            recognizer.close(resolve, reject);
          }), DISPOSE_TIMEOUT_MS, 'Timed out disposing the speech recognizer.');
        } catch (error) {
          recordFailure(error);
        }
        // The owned push stream is already closed and the recognizer disposed.
        // Do not also close the AudioConfig wrapper: Speech SDK 1.51 calls
        // source.turnOff().then(), but its push source returns undefined. That
        // redundant cleanup throws even after a completely successful drain.
      }
      if (failure) throw failure;
    });
    return stopPromise;
  };

  recognizer.recognizing = (_, event) => {
    if (closed || stopping) return;
    const text = event.result.text.trim();
    if (text) {
      options.onPartialText?.(text);
    }
  };

  recognizer.recognized = (_, event) => {
    if (closed || sessionEnded) return;
    if (event.result.reason === SpeechSDK.ResultReason.RecognizedSpeech) {
      const text = event.result.text.trim();
      if (text) {
        // Evaluate against the PCM anchors now, not after async store writes.
        const offsetSeconds = timeline.offsetSeconds(Number(event.result.offset));
        const pending = Promise.resolve()
          .then(() => options.onFinalText(text, offsetSeconds))
          .catch((error: unknown) => {
            recordFailure(error);
          });
        pendingFinals.add(pending);
        void pending.then(() => pendingFinals.delete(pending), () => pendingFinals.delete(pending));
      }
    }
  };

  recognizer.canceled = (_, event) => {
    if (closed) return;
    // Natural EOF is reported as a cancellation BEFORE sessionStopped. It is
    // not an error, nor is it the signal that permits disposal yet.
    if (event.reason === SpeechSDK.CancellationReason.EndOfStream && inputClosed) return;
    recordFailure(event.errorDetails || 'Recognition was canceled before input finished.');
    finishSession();
    void stop().catch(() => { /* also surfaced by onError and subsequent stop() */ });
  };

  recognizer.sessionStopped = () => {
    if (closed) return;
    if (!inputClosed) recordFailure('Recognition stopped before input was closed.');
    finishSession();
    void stop().catch(() => { /* subsequent stop() retains the same outcome */ });
  };

  try {
    await withDeadline(Promise.race([
      new Promise<void>((resolve, reject) => {
        recognizer.startContinuousRecognitionAsync(resolve, reject);
      }),
      sessionEnd.then(() => { throw failure ?? new Error('Recognition ended while starting.'); }),
    ]), START_TIMEOUT_MS, 'Timed out starting speech recognition.');
    // Connection failures can emit canceled and then invoke the start success
    // callback, rather than its error callback, in the installed SDK.
    if (failure) throw failure;
  } catch (error) {
    recordFailure(error);
    finishSession();
    await stop().catch(() => { /* preserve the original failure */ });
    throw failure;
  }

  return {
    pushPcmBase64: (pcmBase64: string, offsetSeconds?: number) => {
      if (!closed && !stopping) {
        const bytes = decodeBase64(pcmBase64);
        const skipSamples = timeline.append(bytes.byteLength, offsetSeconds);
        if (skipSamples * 2 < bytes.byteLength) {
          pushStream.write(skipSamples ? bytes.slice(skipSamples * 2) : bytes);
        }
      }
    },
    stop,
  };
}