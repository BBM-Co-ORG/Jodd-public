// Text from a host is untrusted: backtick-quoted spans become <code>, and
// nothing else in it is markup. Components render each segment as text (never
// {@html}), so a `<b>` in the host's words stays literal.

export type CodeSegment = { text: string; code: boolean };

/** Splits on backticks: odd-indexed pieces are code. An unpaired trailing
 *  backtick leaves its tail as a code segment, as a plain split would. */
export function codeSegments(text: string): CodeSegment[] {
  return text.split('`').map((seg, i) => ({ text: seg, code: i % 2 === 1 }));
}
