import type * as SpeechSDKType from 'microsoft-cognitiveservices-speech-sdk';
import { getAzureCliAccessToken } from './nativeClient';

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
  pushPcmBase64: (pcmBase64: string) => void;
  stop: () => Promise<void>;
};

function decodeBase64(base64: string): ArrayBuffer {
  const binary = window.atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes.buffer;
}

function resultOffsetSeconds(result: SpeechSDKType.SpeechRecognitionResult): number {
  return Math.max(0, Number(result.offset) / 10_000_000);
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
  speechConfig.speechRecognitionLanguage = options.language || 'en-US';
  speechConfig.setProperty(SpeechSDK.PropertyId.Speech_SegmentationStrategy, 'Semantic');

  const audioFormat = SpeechSDK.AudioStreamFormat.getWaveFormatPCM(16000, 16, 1);
  const pushStream = SpeechSDK.AudioInputStream.createPushStream(audioFormat);
  const audioConfig = SpeechSDK.AudioConfig.fromStreamInput(pushStream);
  const recognizer = new SpeechSDK.SpeechRecognizer(speechConfig, audioConfig);
  let closed = false;

  const closeRecognizer = () => {
    if (!closed) {
      closed = true;
      recognizer.close();
    }
  };

  recognizer.recognizing = (_, event) => {
    const text = event.result.text.trim();
    if (text) {
      options.onPartialText?.(text);
    }
  };

  recognizer.recognized = (_, event) => {
    if (event.result.reason === SpeechSDK.ResultReason.RecognizedSpeech) {
      const text = event.result.text.trim();
      if (text) {
        void options.onFinalText(text, resultOffsetSeconds(event.result));
      }
    }
  };

  recognizer.canceled = (_, event) => {
    if (event.reason === SpeechSDK.CancellationReason.Error) {
      options.onError?.(event.errorDetails || 'Azure Speech recognition was canceled.');
    }
  };

  recognizer.startContinuousRecognitionAsync(
    () => undefined,
    (error) => {
      options.onError?.(String(error) || 'Azure Speech recognition could not start.');
      closeRecognizer();
    },
  );

  return {
    pushPcmBase64: (pcmBase64: string) => {
      if (!closed) {
        pushStream.write(decodeBase64(pcmBase64));
      }
    },
    stop: () => new Promise<void>((stopResolve) => {
      pushStream.close();
      if (closed) {
        stopResolve();
        return;
      }
      recognizer.stopContinuousRecognitionAsync(
        () => {
          closeRecognizer();
          stopResolve();
        },
        () => {
          closeRecognizer();
          stopResolve();
        },
      );
    }),
  };
}