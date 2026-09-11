// Deterministic PCM silence, never microphone/speaker data. Keep the original
// replay suite's sample/tail contract so it can also run fully synthetically.
export function syntheticWire() {
  let samples = 0;
  const records = Array.from({ length: 142 }, (_, index) => {
    const count = index < 140 ? 4000 : index === 140 ? 367 : 10400;
    const record = {
      meetingId: 'synthetic-silence-only',
      offsetSeconds: samples / 16000,
      pcmBase64: Buffer.alloc(count * 2).toString('base64'),
    };
    samples += count;
    return JSON.stringify(record);
  });
  return Buffer.from(`${records.join('\n')}\n`);
}