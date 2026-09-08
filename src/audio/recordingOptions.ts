/** Canonical casing for stored BCP-47 tags; accepts legacy lowercase/underscore tags. */
export function normalizeLanguage(value?: string | null, fallback = 'en'): string {
  const tag = value?.trim().replace(/_/g, '-') || fallback;
  return tag.split('-').map((part, index) => {
    if (index === 0) return part.toLowerCase();
    if (/^[a-z]{2}$/i.test(part)) return part.toUpperCase();
    if (/^[a-z]{4}$/i.test(part)) return part[0].toUpperCase() + part.slice(1).toLowerCase();
    return part.toLowerCase();
  }).join('-');
}

/** Azure requires a locale, not the bare language codes used by Foundry. */
export function normalizeAzureLanguage(value?: string | null): string {
  const language = normalizeLanguage(value, 'en-US');
  const defaults: Record<string, string> = {
    auto: 'en-US', en: 'en-US', ko: 'ko-KR', ja: 'ja-JP', zh: 'zh-CN',
    es: 'es-ES', fr: 'fr-FR', de: 'de-DE',
  };
  return Object.prototype.hasOwnProperty.call(defaults, language) ? defaults[language] : language;
}

export function normalizeFoundryLanguage(value?: string | null): string {
  return normalizeLanguage(value).split('-')[0];
}

export const LANGUAGE_OPTIONS = [
  { value: 'auto', label: 'Auto detect' },
  { value: 'en', label: 'English' },
  { value: 'ko', label: 'Korean' },
  { value: 'ja', label: 'Japanese' },
  { value: 'zh', label: 'Chinese' },
  { value: 'es', label: 'Spanish' },
  { value: 'fr', label: 'French' },
  { value: 'de', label: 'German' },
];

export const AZURE_LANGUAGE_OPTIONS = [
  { value: 'en-US', label: 'English' },
  { value: 'ko-KR', label: 'Korean' },
  { value: 'ja-JP', label: 'Japanese' },
  { value: 'zh-CN', label: 'Chinese' },
  { value: 'es-ES', label: 'Spanish' },
  { value: 'fr-FR', label: 'French' },
  { value: 'de-DE', label: 'German' },
];

export const CAPTURE_MODE_OPTIONS = [
  { value: 'microphoneSystem', label: 'Microphone + system' },
  { value: 'microphone', label: 'Microphone only' },
  { value: 'system', label: 'System audio only' },
];
