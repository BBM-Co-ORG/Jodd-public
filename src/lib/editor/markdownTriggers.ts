// Markdown-style typing triggers (`# `, `- `, `1. `, `> ` …). Extracted from
// NoteEditor.svelte so the matching and the "was this a live keystroke?" gate
// can be unit tested without mounting the editor.
//
// Two rules, each learned from a measured failure in real WebKit (2026-09-14):
//
// 1. A trigger fires ONLY on a live space keystroke. The editor used to run
//    detection after every `onInput` — paste, undo, redo, a checkbox click —
//    so pasting "> " at the start of a line produced a blockquote, contrary to
//    the shortcut cheatsheet ("never on paste").
//
// 2. Every DOM change a trigger makes goes through `execCommand`, never direct
//    DOM surgery. WebKit's undo history is a list of edit commands, not DOM
//    snapshots; a `deleteContents()` / `replaceChild()` it never saw leaves
//    that history describing a document that no longer exists. Measured: after
//    `- item`, Cmd+Z then Cmd+Shift+Z produced `<ul><li>item</li></ul>- ` —
//    the list AND the literal `- ` it had replaced.

export type TriggerKind = 'h1' | 'h2' | 'h3' | 'blockquote' | 'ul' | 'ol';

export interface MarkdownTrigger {
  kind: TriggerKind;
  // Characters on the line that make up the trigger, trailing space included —
  // what has to be deleted once the block is converted.
  length: number;
}

// The space may arrive as U+00A0: WebKit types a trailing space into an
// otherwise-empty text run as &nbsp;.
const TRIGGERS: ReadonlyArray<{ match: RegExp; kind: TriggerKind }> = [
  { match: /^#[  ]$/, kind: 'h1' },
  { match: /^##[  ]$/, kind: 'h2' },
  { match: /^###[  ]$/, kind: 'h3' },
  { match: /^>[  ]$/, kind: 'blockquote' },
  { match: /^[-*][  ]$/, kind: 'ul' },
  { match: /^1\.[  ]$/, kind: 'ol' },
];

export function matchTrigger(lineText: string): MarkdownTrigger | null {
  const hit = TRIGGERS.find((t) => t.match.test(lineText));
  return hit ? { kind: hit.kind, length: lineText.length } : null;
}

// True only for the input event of a single typed space. Measured in WebKit:
// a typed space is `insertText` with data " "; a pasted "> " is `insertText`
// with data "> " (so data, not inputType alone, tells them apart); undo and
// redo are `historyUndo` / `historyRedo`. Callers that invoke `onInput()`
// without an event (paste, checkbox, find/replace) are never keystrokes.
export function isTriggerKeystroke(e: Event | undefined): boolean {
  if (!e || typeof (e as InputEvent).inputType !== 'string') return false;
  const ie = e as InputEvent;
  return ie.inputType === 'insertText' && (ie.data === ' ' || ie.data === ' ');
}

const INLINE_TAGS = new Set(['SPAN', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'A', 'CODE', 'FONT']);

// The current line's text up to the caret: walks back through inline siblings
// and stops at a <br> or a block boundary. `block.textContent` would not do —
// Shift+Enter puts several lines in one block, and the regex would then never
// match.
export function lineBeforeCaret(text: Text, offset: number): string {
  let acc = (text.textContent || '').slice(0, offset);
  let cur: Node | null = text.previousSibling;
  while (cur) {
    if (cur.nodeType === 3) acc = (cur.textContent || '') + acc;
    else if (cur.nodeType === 1) {
      const tag = (cur as Element).tagName;
      if (tag === 'BR' || !INLINE_TAGS.has(tag)) break;
      acc = (cur.textContent || '') + acc;
    } else break;
    cur = cur.previousSibling;
  }
  return acc;
}

export interface AppliedTrigger {
  kind: TriggerKind;
  // The exact characters removed (the space may be U+00A0), so a revert puts
  // back what the user typed rather than a normalised copy.
  marker: string;
}

function convertBlock(kind: TriggerKind) {
  if (kind === 'ul') document.execCommand('insertUnorderedList');
  else if (kind === 'ol') document.execCommand('insertOrderedList');
  else document.execCommand('formatBlock', false, `<${kind}>`);
}

// Runs from the editor's `input` handler. Returns what it applied, or null.
export function applyTriggerAtCaret(editorEl: HTMLElement, e: Event | undefined): AppliedTrigger | null {
  if (!isTriggerKeystroke(e)) return null;
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  if (!range.collapsed || !editorEl.contains(range.startContainer)) return null;

  // The caret may sit on an element boundary just after the text node that
  // holds the marker; resolve that to the text node itself.
  let node: Node = range.startContainer;
  let offset = range.startOffset;
  if (node.nodeType !== 3) {
    const prev = offset > 0 ? node.childNodes[offset - 1] : null;
    if (!prev || prev.nodeType !== 3) return null;
    node = prev;
    offset = (prev as Text).length;
  }
  if (node.parentElement?.closest('pre, code')) return null;

  const marker = lineBeforeCaret(node as Text, offset);
  const trigger = matchTrigger(marker);
  if (!trigger) return null;

  // Convert FIRST, while the line still holds the marker: WebKit's formatBlock
  // silently no-ops on an empty block, which is why the old code bypassed
  // execCommand with a hand-built replaceChild. Converting a non-empty line
  // also keeps the caret after the marker, so the delete below needs no DOM
  // walk to find it.
  convertBlock(trigger.kind);
  for (let i = 0; i < marker.length; i++) sel.modify('extend', 'backward', 'character');
  document.execCommand('delete');
  return { kind: trigger.kind, marker };
}

// Backspace immediately after a trigger: put the marker back and undo the
// conversion. Insert FIRST so the block is not empty when it is converted
// back (the same formatBlock quirk). Inserting the whole marker in one
// `insertText` also means the input event it fires carries "# ", not a lone
// space, so the trigger cannot immediately re-fire.
export function revertTrigger(applied: AppliedTrigger) {
  document.execCommand('insertText', false, applied.marker);
  if (applied.kind === 'ul') document.execCommand('insertUnorderedList');
  else if (applied.kind === 'ol') document.execCommand('insertOrderedList');
  else document.execCommand('formatBlock', false, '<div>');
}

const CHECKBOX_HTML = '<input type="checkbox" contenteditable="false">&nbsp;';

// Measured in WebKit: `insertHTML` drops `contenteditable` from the markup it
// inserts, and redo re-creates an inserted checkbox without it too. Without
// `contenteditable="false"` a click on the box places the caret instead of
// toggling it (see formatTask). Setting an attribute is not an edit command,
// so repairing it here cannot desynchronise the undo history the way node
// surgery did — call it after anything that may have inserted a checkbox.
export function ensureCheckboxesNotEditable(root: ParentNode) {
  root
    .querySelectorAll('input[type=checkbox]:not([contenteditable="false"])')
    .forEach((cb) => cb.setAttribute('contenteditable', 'false'));
}

function caretToEndOf(el: Element) {
  const r = document.createRange();
  r.selectNodeContents(el);
  r.collapse(false);
  const sel = window.getSelection();
  sel?.removeAllRanges();
  sel?.addRange(r);
}

// Enter on a non-empty checklist row: a new unchecked row below it, carrying
// the row's indent (insertParagraph clones the block's attributes).
export function continueChecklist(row: HTMLElement) {
  caretToEndOf(row);
  document.execCommand('insertParagraph');
  document.execCommand('insertHTML', false, CHECKBOX_HTML);
  ensureCheckboxesNotEditable(row.parentElement ?? row);
}

// Enter on an empty checklist row: it stops being a checklist row.
export function exitChecklist(row: HTMLElement) {
  const r = document.createRange();
  r.selectNodeContents(row);
  const sel = window.getSelection();
  sel?.removeAllRanges();
  sel?.addRange(r);
  document.execCommand('delete');
}
