// The Tauri webview's clipboard, for component tests of a Copy button.
//
// `navigator.clipboard.writeText` rejects in the real window (live run,
// AccountPage), so `copyText` falls back to selecting a hidden textarea and
// `execCommand('copy')`. jsdom has no execCommand; this stub records what the
// textarea held when the copy ran, or refuses it.
//
//   'rejects' — writeText rejects, the fallback copies (the live case)
//   'fails'   — writeText rejects and execCommand refuses: nothing copied
import { vi } from 'vitest';

export interface ClipboardStub {
  writeText: ReturnType<typeof vi.fn>;
  /** What the fallback copied, in order. */
  copied: string[];
  restore(): void;
}

export function stubWebviewClipboard(mode: 'rejects' | 'fails'): ClipboardStub {
  const copied: string[] = [];
  const writeText = vi.fn(() => Promise.reject(new DOMException('Write permission denied.', 'NotAllowedError')));
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
  Object.defineProperty(document, 'execCommand', {
    configurable: true,
    writable: true,
    value: (cmd: string) => {
      if (cmd !== 'copy' || mode === 'fails') return false;
      const ta = document.body.querySelector(':scope > textarea') as HTMLTextAreaElement | null;
      if (!ta) return false;
      copied.push(ta.value);
      return true;
    },
  });
  return {
    writeText,
    copied,
    restore() {
      delete (navigator as unknown as Record<string, unknown>).clipboard;
      delete (document as unknown as Record<string, unknown>).execCommand;
    },
  };
}
