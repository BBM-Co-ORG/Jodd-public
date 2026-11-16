// How much a note takes on this device: its stored title and HTML body, plus
// the attachments the cache holds for it (get_note_attachments' `size`). The
// date tooltip's last line. It is the local copy's size — the remote's can
// differ (Gmail stores a MIME message, attachments base64-encoded; iCloud a
// compressed document), so the line says "on this device".

/** Bytes the cache stores for the note's text: UTF-8, as SQLite keeps it. */
export function noteTextBytes(title: string, bodyHtml: string): number {
  const enc = new TextEncoder();
  return enc.encode(title ?? '').length + enc.encode(bodyHtml ?? '').length;
}

/** "512 bytes", "4.2 KB", "48 KB", "1.2 MB" — binary units, as Finder does not
 *  but every byte-counting tool in the app does (Records, Restore). */
export function formatBytes(n: number): string {
  if (n < 1024) return n === 1 ? '1 byte' : `${n} bytes`;
  const kb = n / 1024;
  if (kb < 1024) return `${kb < 10 ? kb.toFixed(1) : Math.round(kb)} KB`;
  const mb = kb / 1024;
  return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
}

export interface AttachmentSize {
  count: number;
  bytes: number;
}

/** The tooltip's size line. `attachments` is null until the editor has read
 *  them for this note (or when it could not): the text alone is shown then. */
export function sizeLine(textBytes: number, attachments: AttachmentSize | null): string {
  if (!attachments || attachments.count === 0) return `${formatBytes(textBytes)} on this device`;
  const total = textBytes + attachments.bytes;
  const what = attachments.count === 1 ? '1 attachment' : `${attachments.count} attachments`;
  return `${formatBytes(total)} on this device (note ${formatBytes(textBytes)} · ${what} ${formatBytes(attachments.bytes)})`;
}
