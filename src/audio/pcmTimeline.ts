const SAMPLE_RATE = 16_000;

type TimelineAnchor = { streamSample: number; recordingSample: number };

/** Maps the SDK's compact input clock to saved PCM, including skipped capture
 * intervals. A new recognizer gets a new instance and its first chunk sets the
 * absolute base. Missing offsets retain the original zero-based API behavior.
 */
export class PcmTimeline {
  private streamSamples = 0;
  private recordingEndSample: number | undefined;
  private readonly anchors: TimelineAnchor[] = [];

  /** Register mono PCM16 bytes. Returns leading overlapping samples to skip. */
  append(byteLength: number, offsetSeconds?: number): number {
    if (!Number.isSafeInteger(byteLength) || byteLength < 0 || byteLength % 2 !== 0) {
      throw new Error('Azure Speech requires complete PCM16 samples.');
    }
    if (offsetSeconds !== undefined && (!Number.isFinite(offsetSeconds) || offsetSeconds < 0)) {
      throw new Error('PCM offset must be a finite, non-negative number.');
    }
    const sampleCount = byteLength / 2;
    if (sampleCount === 0) return 0;
    const startSample = offsetSeconds === undefined
      ? this.recordingEndSample ?? 0
      : Math.round(offsetSeconds * SAMPLE_RATE);
    if (!Number.isSafeInteger(startSample + sampleCount)) {
      throw new Error('PCM offset exceeds the supported timeline.');
    }
    const skipSamples = Math.min(sampleCount, Math.max(0, (this.recordingEndSample ?? startSample) - startSample));
    if (skipSamples === sampleCount) return skipSamples;
    const acceptedStart = startSample + skipSamples;
    if (this.recordingEndSample === undefined || acceptedStart !== this.recordingEndSample) {
      this.anchors.push({ streamSample: this.streamSamples, recordingSample: acceptedStart });
    }
    this.streamSamples += sampleCount - skipSamples;
    this.recordingEndSample = startSample + sampleCount;
    return skipSamples;
  }

  /** SDK offsets are 100ns ticks relative to the first PCM it received. */
  offsetSeconds(offsetTicks: number): number {
    const sample = Math.max(0, Number.isFinite(offsetTicks) ? offsetTicks : 0) * SAMPLE_RATE / 10_000_000;
    let low = 0;
    let high = this.anchors.length;
    while (low < high) {
      const mid = Math.floor((low + high) / 2);
      if (this.anchors[mid].streamSample <= sample) low = mid + 1;
      else high = mid;
    }
    const anchor = this.anchors[Math.max(0, low - 1)];
    return anchor
      ? (anchor.recordingSample + sample - anchor.streamSample) / SAMPLE_RATE
      : sample / SAMPLE_RATE;
  }
}