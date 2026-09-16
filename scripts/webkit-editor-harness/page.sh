#!/bin/sh
# Builds the page harness.swift loads: the REAL editor modules
# (src/lib/editor/*.ts), bundled by esbuild, wired the way NoteEditor.svelte
# wires them.
#
# The wiring below is a COPY of NoteEditor's handlers (onInput, the Backspace
# revert and merge, undo/redo, the checklist chord, Enter on a checklist row or
# an empty quote/heading line) and of the bodies of addNormalizedTag,
# removeTag, replaceCurrent, replaceAll and pickLink, exposed as window.*
# for keys/ scripts to call. Change those and this copy must follow, or the
# harness measures something the app no longer does.
#
# Usage: page.sh <out-dir>   → writes <out-dir>/page.html
set -e
OUT=$1
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
: > "$OUT/entry.ts"
for m in markdownTriggers shortcutKeys exitSpecialBlock backspaceMergeEmpty bodyEdits; do
  printf "export * from '%s/src/lib/editor/%s';\n" "$ROOT" "$m" >> "$OUT/entry.ts"
done
"$ROOT/node_modules/.bin/esbuild" "$OUT/entry.ts" --bundle --format=iife --global-name=MT --log-level=warning > "$OUT/mt.js"
{
echo '<html><body><div id="ed" contenteditable="true" style="min-height:200px"></div><script>'
cat "$OUT/mt.js"
cat <<'JS'
const ed = document.getElementById('ed');
function caretDesc() { const s = getSelection(); if (!s.rangeCount) return '-'; const r = s.getRangeAt(0); const n = r.startContainer; return (n.nodeType === 3 ? JSON.stringify(n.data) : n.nodeName) + '@' + r.startOffset; }
function placeCaret(el, atStart) { const r = document.createRange(); r.selectNodeContents(el); r.collapse(!!atStart); const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
function topBlock(el) { let cur = el; while (cur && cur !== ed) { if (['DIV','LI','P','H1','H2','H3','H4','BLOCKQUOTE'].includes(cur.tagName)) return cur; cur = cur.parentElement; } return null; }
function taskBlock() { const sel = getSelection(); if (!sel.rangeCount) return null; const node = sel.getRangeAt(0).startContainer; const el = node.nodeType === 3 ? node.parentElement : node; const b = topBlock(el); return b && b.querySelector(':scope > input[type=checkbox]') ? b : null; }
let lastTrigger = null;
function onInput(e) { MT.ensureCheckboxesNotEditable(ed); requestAnimationFrame(() => { const a = MT.applyTriggerAtCaret(ed, e); if (a) lastTrigger = a; }); }
ed.addEventListener('input', onInput);
ed.addEventListener('keydown', (e) => {
  if (lastTrigger && e.key === 'Backspace' && !e.metaKey && !e.ctrlKey && !e.altKey) { e.preventDefault(); const a = lastTrigger; lastTrigger = null; MT.revertTrigger(a); return; }
  if (lastTrigger && !['Shift','Meta','Control','Alt'].includes(e.key)) lastTrigger = null;
  if (e.key === 'Backspace' && !e.metaKey && !e.ctrlKey && !e.altKey && MT.tryBackspaceMergeEmptyPrevious(ed)) { e.preventDefault(); onInput(); return; }
  const mod = MT.isShortcutMod(e, true); // macOS: the only platform this harness runs on
  if (mod && !e.altKey) {
    if (!e.shiftKey && e.code === 'KeyZ') { e.preventDefault(); document.execCommand('undo'); onInput(); return; }
    if (e.shiftKey && e.code === 'KeyZ') { e.preventDefault(); document.execCommand('redo'); onInput(); return; }
    // Probes, not features: keys/mac.txt checks that Ctrl+B / Ctrl+E on macOS
    // reach the system text bindings instead of these.
    if (!e.shiftKey && e.code === 'KeyB') { e.preventDefault(); document.execCommand('bold'); return; }
    if (!e.shiftKey && e.code === 'KeyE') { e.preventDefault(); document.execCommand('insertHTML', false, '<code>CODE</code>'); return; }
    if (e.shiftKey && e.code === 'Digit9') { e.preventDefault(); window.formatTask(); return; }
  }
  if (e.key === 'Enter' && !e.shiftKey) {
    const tb = taskBlock();
    if (tb) {
      e.preventDefault();
      if ((tb.textContent || '').replace(/[\s ]/g, '') === '') MT.exitChecklist(tb); else MT.continueChecklist(tb);
      onInput();
      return;
    }
    if (!e.metaKey && !e.ctrlKey && !e.altKey && MT.tryExitSpecialBlock(ed)) { e.preventDefault(); onInput(); return; }
  }
});

// NoteEditor bodies that edit the note from outside a keystroke in it.
window.formatTask = () => { MT.insertChecklist(ed); onInput(); };
window.addTag = (tag) => { MT.appendTagToBody(ed, tag); onInput(); };
window.removeTag = (tag) => { MT.replaceRanges(ed, MT.findTextRanges(ed, MT.hashtagMatches(tag)), ''); onInput(); };
// replaceCurrent acts on the current match; scripts use the first one.
window.replaceCurrent = (q, rep) => { const m = MT.findTextRanges(ed, MT.substringMatches(q))[0]; if (m) MT.replaceRanges(ed, [m], rep); onInput(); };
window.replaceAll = (q, rep) => { MT.replaceRanges(ed, MT.findTextRanges(ed, MT.substringMatches(q)), rep); onInput(); };
// pickLink replaces the "[[query" its link anchor points at.
window.pickLink = (query, slug) => { const r = MT.findTextRanges(ed, MT.substringMatches('[[' + query))[0]; MT.replaceRanges(ed, [r], '[[' + slug + ']]', { giveBackFocus: false }); ed.focus(); onInput(); };

window.snapshot = () => ed.innerHTML + '   caret=' + caretDesc();
// INITIAL: the body. CARET: optional [selector, 'start' | 'end']; default is
// the end of the last child.
window.setup = () => {
  ed.innerHTML = window.INITIAL || '<div>first line</div><div><br></div>';
  ed.focus();
  const c = window.CARET;
  placeCaret(c ? ed.querySelector(c[0]) : ed.lastChild, c && c[1] === 'start');
};
JS
echo '</script></body></html>'
} > "$OUT/page.html"
