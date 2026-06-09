<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount, onDestroy, tick } from 'svelte';
  import { get } from 'svelte/store';
  import { selectedNote, notes, isSaving, error, currentAccount, markRecentlySaved, indexUpsertOnSave, indexRemoveOnDelete } from '../stores/notes';
  import type { Note } from '../types';

  let title = '';
  let editorEl: HTMLDivElement;
  let saveTimer: ReturnType<typeof setTimeout>;

  // uuid of the note currently rendered into editorEl. We repopulate the
  // contenteditable DOM when (a) the user switches to a different note, or
  // (b) the SAME note's body changed externally — e.g. polling picked up
  // Apple Notes' edit. Tracked separately from selectedNote.body_html so we
  // can tell "this is fresh server content" from "this is the body we wrote".
  let editorUuid: string | undefined;
  let lastRenderedBody: string | undefined;
  // Last (body, title) confirmed as pushed to Gmail. Distinct from
  // lastRenderedBody because lastRenderedBody is updated on every onInput
  // keystroke (for optimistic store sync), so it equals the live editor —
  // useless for "has anything actually changed vs server" comparisons.
  // These two ONLY move forward when a save succeeds (or on initial render
  // from cached/pulled content, which is the server's truth at that moment).
  let lastPushedBody: string | undefined;
  let lastPushedTitle: string | undefined;

  // The Note object the editor is currently bound to. Distinct from
  // $selectedNote because $selectedNote may have already changed to a NEW
  // note inside the reactive block — we need the OLD note's identity to
  // flush a pending save before the swap.
  let editorNote: Note | null = null;

  // Re-render decisions:
  //   - UUID changed → user selected a different note → always re-render,
  //     but flush any pending edit for the OLD note first (or it's lost forever)
  //   - Same UUID but body changed externally (poll picked up Apple Notes' edit)
  //     → re-render, BUT only when safe (not saving, not typing)
  //   - Same UUID, body matches what we last wrote out → no re-render
  $: if ($selectedNote) {
    const uuidChanged = $selectedNote.uuid !== editorUuid;
    const bodyChanged = $selectedNote.body_html !== lastRenderedBody;
    // Treat ANY element inside the editor as "user is interacting" — not
    // just the editor div itself. Clicking a checkbox shifts focus to the
    // <input>; the original check (activeElement === editorEl) missed
    // that and let polls re-render the body, wiping the user's toggle.
    const activeIsInEditor =
      typeof document !== 'undefined' &&
      !!editorEl &&
      document.activeElement instanceof Node &&
      (document.activeElement === editorEl ||
        editorEl.contains(document.activeElement));
    const shouldRender = uuidChanged || (bodyChanged && !$isSaving && !activeIsInEditor);

    if (shouldRender) {
      // FLUSH then clear — never just clear. If we're switching away from a
      // note that has a pending unsaved edit, the user's typing only exists
      // in DOM/title-variable; canceling the autosave timer without firing
      // a save would silently discard it. Capture the previous note's
      // identity + current editor contents and fire a save in the background.
      if (uuidChanged && editorNote && editorEl) {
        const prevBody = wrapBody(editorEl.innerHTML);
        flushPendingEdit(editorNote, title, prevBody);
        // If the previous note was an untouched tmp: blank (default title,
        // no real body content), drop it from the notes array. Otherwise
        // every `+` click without typing leaves a permanent ghost row in
        // the sidebar, which is how 8 real notes became 15 visible rows.
        if (
          editorNote.uuid?.startsWith('tmp:') &&
          title === 'New Note'
        ) {
          const inner = prevBody.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? '';
          const stripped = inner.replace(/<[^>]*>/g, '').replace(/\s|&nbsp;/g, '');
          if (stripped === '') {
            const dropUuid = editorNote.uuid;
            notes.update((ns) => ns.filter((n) => n.uuid !== dropUuid));
          }
        }
      }
      clearTimeout(saveTimer);
      editorNote = $selectedNote;
      editorUuid = $selectedNote.uuid;
      title = typeof $selectedNote.title === 'string' ? $selectedNote.title : '';
      // Defer innerHTML until after Svelte's DOM update (bind:this timing).
      const targetUuid = $selectedNote.uuid;
      const targetBody = $selectedNote.body_html;
      tick().then(() => {
        if (editorEl && editorUuid === targetUuid) {
          editorEl.innerHTML = extractBody(targetBody);
          lastRenderedBody = targetBody;
          // Initial render = the server's confirmed state for this note.
          // Anchor the "did anything change since push?" comparison here.
          lastPushedBody = targetBody;
          lastPushedTitle = title;
        }
      });
    }
  } else {
    // No note selected → flush any pending edit before clearing state
    if (editorNote && editorEl) {
      flushPendingEdit(editorNote, title, wrapBody(editorEl.innerHTML));
    }
    clearTimeout(saveTimer);
    editorNote = null;
    editorUuid = undefined;
    lastRenderedBody = undefined;
  }

  // Fire-and-forget save with an explicit note identity (not $selectedNote).
  // Called when the user switches notes before the debounced autosave fires —
  // the captured `note` parameter carries the previous note's id/uuid/label
  // so the save targets the right Gmail message, not whichever note is
  // currently selected after the swap.
  function flushPendingEdit(note: Note, noteTitle: string, bodyHtml: string) {
    // Skip if nothing has changed since the last confirmed push (same logic
    // as autoSave). Compare against lastPushedBody/Title, NOT lastRenderedBody
    // — lastRenderedBody moves on every keystroke (see autoSave for the full
    // reasoning).
    if (bodyHtml === lastPushedBody && noteTitle === lastPushedTitle) return;
    // Skip if this is an untouched brand-new note (tmp: UUID, default title,
    // no real body content). The wrap-on-flush adds a style attribute that
    // makes the string compare above false-positive — without this guard,
    // clicking + and then clicking another note saves a blank "New Note"
    // to Gmail. We use the tmp: prefix as proof the note has never been
    // persisted; for real notes we always honor the user's edits (even
    // emptying them deliberately).
    if (note.uuid?.startsWith('tmp:') && noteTitle === 'New Note') {
      const inner = bodyHtml.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? '';
      const stripped = inner.replace(/<[^>]*>/g, '').replace(/\s|&nbsp;/g, '');
      if (stripped === '') return;
    }
    // Don't block the swap. Save runs concurrently with the new note rendering.
    saveNote(note, noteTitle, bodyHtml).catch((e) => {
      error.set(`flush save failed: ${e}`);
    });
  }

  function extractBody(html: string): string {
    const match = html.match(/<body[^>]*>([\s\S]*)<\/body>/i);
    return match ? match[1] : html;
  }

  function wrapBody(inner: string): string {
    return `<html><head></head><body style="overflow-wrap: break-word; -webkit-nbsp-mode: space; line-break: after-white-space;">${inner}</body></html>`;
  }

  function onInput() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(autoSave, 1500);

    // Optimistic store update: reflect the live edit in the note list immediately
    // (title chip, body preview) without waiting for the 1.5s autosave debounce.
    // The actual Gmail save still goes through autoSave; this just keeps the
    // user-visible store in sync with what they're typing.
    //
    // We update lastRenderedBody to the same value we're putting in the store,
    // so the editor's re-render reactive block doesn't see this as an "external
    // change" and try to re-render (which would destroy the cursor).
    if ($selectedNote && editorEl) {
      const liveBody = wrapBody(editorEl.innerHTML);
      const liveTitle = typeof title === 'string' ? title : '';
      // Skip if nothing actually changed since last render.
      if (liveBody === lastRenderedBody && liveTitle === $selectedNote.title) return;
      const updated: Note = {
        ...$selectedNote,
        title: liveTitle,
        body_html: liveBody,
      };
      lastRenderedBody = liveBody;
      notes.update((ns) => {
        const idx = ns.findIndex((n) => n.uuid === updated.uuid);
        if (idx >= 0) ns[idx] = updated;
        return ns;
      });
      selectedNote.set(updated);
    }
  }

  function onTitleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      editorEl?.focus();
    }
    // DO NOT call onInput() here. The title input already has `oninput={onInput}`
    // which fires only when the *value* actually changes. Calling onInput from
    // keydown would schedule autosave on every key — including navigation keys
    // (Tab, arrows, Esc) and modifier shortcuts (Cmd+C etc) that don't change
    // the value but would still trigger a save 1.5s later. That was the silent
    // path that allowed an empty-editor autosave to fire without any real edit.
  }

  async function autoSave() {
    if (!$selectedNote) return;
    const bodyHtml = wrapBody(editorEl?.innerHTML || '');

    // Skip if nothing has changed since the LAST CONFIRMED PUSH to Gmail.
    // The 1.5s debounce reschedules on every keystroke, so type+delete leaves
    // a save pending with byte-identical content; without this guard Jodd
    // would insert a new Gmail message + trash the prior one for zero net
    // change. Compare against lastPushedBody/lastPushedTitle (server truth),
    // NOT lastRenderedBody — that one tracks the live editor and would
    // always equal `bodyHtml`, falsely skipping legitimate edits.
    const titleStr = typeof title === 'string' ? title : '';
    if (bodyHtml === lastPushedBody && titleStr === lastPushedTitle) return;

    // ─── Data-loss guard ────────────────────────────────────────────────
    const stored = $selectedNote.body_html || '';
    const storedText = stored.replace(/<[^>]*>/g, ' ').replace(/&nbsp;/g, ' ').replace(/\s+/g, ' ').trim();
    const newText = (editorEl?.innerText || '').trim();
    if ($selectedNote.id && storedText.length >= 20) {
      const ratio = newText.length / storedText.length;
      if (ratio < 0.25) {
        error.set(
          `Autosave blocked: would shrink note from ${storedText.length} to ${newText.length} chars. ` +
            `If you intended to clear the note, delete it instead.`
        );
        return;
      }
    }
    // ────────────────────────────────────────────────────────────────────

    await saveNote($selectedNote, title, bodyHtml);
  }

  // saveNote takes an explicit Note parameter rather than reading
  // $selectedNote inline. This matters for two paths:
  //   1) autoSave passes $selectedNote (current note being edited)
  //   2) flushPendingEdit passes the PREVIOUS note (already off-screen because
  //      the user just clicked a different note) — we must save against THAT
  //      identity, not the currently-selected one
  // Serialize all saves through a single Promise chain so multiple in-flight
  // autosaves can't race. Each save sees the previous one's resolved Gmail id
  // (via the updated store) before deciding what to trash — eliminating the
  // "every save tracks the same stale OLD id and orphans the previous NEW id"
  // failure mode that was causing dozens of duplicate messages to accumulate.
  let savePromiseChain: Promise<unknown> = Promise.resolve();

  function saveNote(note: Note, noteTitle: string, bodyHtml: string): Promise<void> {
    const next = savePromiseChain
      .catch(() => {}) // previous failure doesn't block this one
      .then(() => doSaveNote(note, noteTitle, bodyHtml));
    savePromiseChain = next.catch(() => {});
    return next;
  }

  async function doSaveNote(note: Note, noteTitle: string, bodyHtml: string) {
    // Defensive: if any of these are the wrong type, Tauri's serde rejects
    // the invoke with a cryptic "invalid type: map, expected a string" that
    // tells us nothing about which value was wrong. Catch it here with a
    // useful message so we can diagnose.
    if (typeof noteTitle !== 'string') {
      const sample = (() => { try { return JSON.stringify(noteTitle).slice(0, 200); } catch { return String(noteTitle); } })();
      error.set(`saveNote refused: noteTitle is ${typeof noteTitle}, expected string. Value: ${sample}`);
      console.error('saveNote: noteTitle bad type', noteTitle);
      return;
    }
    if (typeof bodyHtml !== 'string') {
      const sample = (() => { try { return JSON.stringify(bodyHtml).slice(0, 200); } catch { return String(bodyHtml); } })();
      error.set(`saveNote refused: bodyHtml is ${typeof bodyHtml}, expected string. Value: ${sample}`);
      console.error('saveNote: bodyHtml bad type', bodyHtml);
      return;
    }
    if (!note || typeof note !== 'object') {
      error.set(`saveNote refused: note is ${typeof note}, expected object`);
      console.error('saveNote: note bad type', note);
      return;
    }

    // Re-read the latest note from the store. If a previous queued save
    // updated this note's Gmail id, we want to use the freshest id — not
    // the one captured when this save was queued. Without this re-read,
    // the queue serialization helps but the second save still tracks the
    // stale id that the first save just replaced.
    const cur = get(selectedNote);
    if (cur && cur.uuid === note.uuid) {
      note = { ...note, id: cur.id, x_mail_created_date: cur.x_mail_created_date ?? note.x_mail_created_date };
    } else {
      const fromArr = get(notes).find((n) => n.uuid === note.uuid);
      if (fromArr) {
        note = { ...note, id: fromArr.id, x_mail_created_date: fromArr.x_mail_created_date ?? note.x_mail_created_date };
      }
    }

    isSaving.set(true);
    error.set(null);
    try {
      const existingGmailId = note.id || null;
      // A `tmp:` prefix marks a client-side temporary uuid for a brand-new
      // note that has never been saved. Strip it before sending — Rust
      // generates a real Apple-format UUID when existing_uuid is empty.
      const existingUuid = note.uuid && !note.uuid.startsWith('tmp:') ? note.uuid : null;
      const tmpUuidToReplace = note.uuid && note.uuid.startsWith('tmp:') ? note.uuid : null;
      const existingXMailCreatedDate = note.x_mail_created_date || null;
      const label = note.label || 'Notes';

      const noteAcct = note.account_id ?? null;
      const curAcct = get(currentAccount);
      const accountId = noteAcct || curAcct;
      if (typeof accountId !== 'string' || accountId.length === 0) {
        error.set(
          `No account context to save into (note.account_id=${JSON.stringify(noteAcct)}, currentAccount=${JSON.stringify(curAcct)}).`,
        );
        return;
      }

      const saved = await invoke<{ id: string; uuid: string }>('save_note', {
        accountId,
        title: noteTitle,
        bodyHtml,
        existingGmailId,
        existingUuid,
        existingXMailCreatedDate,
        label,
      });

      const updatedNote: Note = {
        id: saved.id,
        uuid: saved.uuid,
        title: noteTitle,
        body_html: bodyHtml,
        date: new Date().toISOString(),
        label,
        x_mail_created_date: existingXMailCreatedDate,
        account_id: accountId,
      };

      notes.update(ns => {
        // For a first-time save of a new note, the array contains a tmp:
        // entry that we need to replace with the resolved real UUID — find
        // by either real UUID (subsequent saves) OR tmp UUID (first save).
        const idx = ns.findIndex(
          n => n.uuid === saved.uuid || (tmpUuidToReplace !== null && n.uuid === tmpUuidToReplace),
        );
        if (idx >= 0) {
          ns[idx] = updatedNote;
        } else {
          ns = [updatedNote, ...ns];
        }
        return ns;
      });

      // Protect this note from being overwritten by a background poll that
      // fires before Gmail's index has propagated our insert. App.svelte's
      // loadNotes merges recently-saved notes back in if the fetch missed them.
      // Scoped per-account so Account A's save doesn't suppress Account B's
      // remote change for the same uuid (cross-mailbox notes can collide).
      markRecentlySaved(accountId, saved.uuid);

      // Keep the sidebar folder counts honest. The index is a sign-in
      // snapshot; without this patch a fresh note never bumps its folder.
      indexUpsertOnSave(accountId, existingGmailId, saved.id, label);

      // CRITICAL guard: only mutate selectedNote / lastRenderedBody if the
      // user is STILL viewing the note we just saved. Otherwise a flush-on-
      // switch would yank the user back to the previous note after they've
      // already navigated away — terrible UX.
      const cur = get(selectedNote);
      if (cur?.uuid === note.uuid) {
        // ALSO sync editorUuid/editorNote BEFORE selectedNote.set fires the
        // reactive block. Otherwise: when we save a brand-new note (whose
        // captured uuid was ''), the resulting selectedNote.set(updatedNote)
        // changes $selectedNote.uuid to a real value, the reactive block
        // sees uuidChanged=true and fires flushPendingEdit(blank_note, ...),
        // which invokes save_note AGAIN with empty existing_uuid → Rust
        // generates ANOTHER fresh UUID → a 2× duplicate of the new note.
        // Keeping editorUuid/editorNote in lockstep with selectedNote means
        // the reactive block sees "no change, no flush needed."
        lastRenderedBody = updatedNote.body_html;
        // Anchor the "did anything change since push?" check to what we just
        // confirmed on Gmail. Without this, the next autoSave would compare
        // against lastRenderedBody (which moves on every keystroke) and skip
        // a legitimate edit because "live editor == lastRenderedBody" by
        // construction.
        lastPushedBody = updatedNote.body_html;
        lastPushedTitle = updatedNote.title;
        editorUuid = updatedNote.uuid;
        editorNote = updatedNote;
        selectedNote.set(updatedNote);
      }
    } catch (e) {
      error.set(String(e));
    } finally {
      isSaving.set(false);
    }
  }

  async function deleteNote() {
    const target = $selectedNote;
    if (!target) return;
    // No native confirm() — unreliable in WKWebView. The trash icon click
    // is already an explicit action; deleted notes go to Gmail's Trash
    // folder (30-day recovery), not permanent delete.
    try {
      const accountId = target.account_id || $currentAccount;
      if (!accountId) {
        error.set('No account context — cannot delete.');
        return;
      }
      await invoke('delete_note', {
        accountId,
        id: target.id,
        uuid: target.uuid,
      });
      // Filter by uuid — id can be empty for local-only notes that have
      // never synced, in which case id-based filtering would drop nothing
      // (or every other unsynced note).
      notes.update(ns => ns.filter(n => n.uuid !== target.uuid));
      indexRemoveOnDelete(accountId, target.id);
      selectedNote.set(null);
    } catch (e) {
      error.set(String(e));
    }
  }

  // formatting commands
  function formatBold() { document.execCommand('bold'); }
  function formatItalic() { document.execCommand('italic'); }
  function formatList() { document.execCommand('insertUnorderedList'); }

  // Insert a task line at the cursor: a single <div> with a checkbox + space.
  // We use a <div> wrapper rather than a bare <input> so pressing Enter after
  // the task creates a new line cleanly (contenteditable promotes the new line
  // to a sibling div). The trailing &nbsp; gives the cursor a place to land
  // when the user starts typing — without it, the caret falls behind the input.
  //
  // `contenteditable="false"` on the input itself is critical: inputs inside
  // a contenteditable region are otherwise treated as text-flow content and
  // some browsers swallow clicks for caret placement instead of toggling the
  // form control. Marking it non-editable restores normal click behavior.
  function formatTask() {
    document.execCommand(
      'insertHTML',
      false,
      '<div><input type="checkbox" contenteditable="false">&nbsp;</div>',
    );
    onInput();
  }

  // Click handler for checkboxes inside the editor body.
  //
  // Critical: DO NOT call e.preventDefault(). In WKWebView's contenteditable,
  // preventDefault on a checkbox click suppresses the native visual repaint —
  // the .checked property updates but the box stays painted in its previous
  // state. The earlier version did preventDefault to avoid double-toggling,
  // but the cost was invisible UI updates.
  //
  // Instead: let the native click toggle the property (visual repaints
  // immediately), then sync the `checked` attribute in a microtask so
  // innerHTML serialization captures the new state (innerHTML only serializes
  // the attribute, never the property).
  function onEditorClick(e: MouseEvent) {
    const t = e.target as HTMLElement;
    if (t.tagName === 'INPUT' && (t as HTMLInputElement).type === 'checkbox') {
      // Defer to a microtask so the native click has already flipped
      // .checked. Reading the property at that point tells us the new state.
      queueMicrotask(() => {
        const cb = t as HTMLInputElement;
        if (cb.checked) cb.setAttribute('checked', '');
        else cb.removeAttribute('checked');
        onInput();
      });
    }
  }

  // Updated-date display at top of note (mirrors Apple Notes).
  // Forces *local* timezone (getDate/getMonth/getHours etc are local-tz).
  // For Thailand timezone (+07:00, no DST) we emit the BE calendar form Apple
  // Notes uses: "4 June BE 2569, 02:25". Elsewhere we fall back to the
  // system locale's natural date+time.
  const EN_MONTHS = ['January','February','March','April','May','June',
                     'July','August','September','October','November','December'];

  function formatNoteDate(s: string): string {
    if (!s) return '';
    const d = new Date(s);
    if (isNaN(d.getTime())) return s;

    // getTimezoneOffset returns minutes WEST of UTC, so +07:00 = -420.
    const isThaiTZ = d.getTimezoneOffset() === -420;
    if (isThaiTZ) {
      const day = d.getDate();
      const month = EN_MONTHS[d.getMonth()];
      const beYear = d.getFullYear() + 543;
      const hh = String(d.getHours()).padStart(2, '0');
      const mm = String(d.getMinutes()).padStart(2, '0');
      return `${day} ${month} BE ${beYear}, ${hh}:${mm}`;
    }

    // Other regions: use system locale (24-hour clock).
    const date = d.toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' });
    const time = d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hour12: false });
    return `${date}, ${time}`;
  }

  // ─── Find / Replace ───────────────────────────────────────────────
  // Cmd+F toggles the find bar. Search/replace operate by walking text
  // nodes in the contenteditable — preserves HTML structure, no innerHTML
  // mass-replace that would kill the cursor.
  let showFind = false;
  let findQuery = '';
  let replaceQuery = '';
  let showReplace = false;
  let matchCount = 0;
  let currentMatchIdx = 0;
  let findInputEl: HTMLInputElement | undefined;

  function gatherTextNodes(root: Node): Text[] {
    const out: Text[] = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node) {
      if ((node as Text).data.length > 0) out.push(node as Text);
      node = walker.nextNode();
    }
    return out;
  }

  // A match can live either in the title input or in a body text node.
  // The find/replace UI treats them as a single ordered sequence (title
  // matches first, then body matches in document order) so that nextMatch
  // can navigate between them naturally.
  //
  // Apple Notes folds title-and-body into one editable surface (their
  // first body line IS the title), so find/replace there is body-only.
  // Jodd splits them — extending find/replace to both keeps the user's
  // mental model intact.
  type Match =
    | { kind: 'title'; start: number; end: number }
    | { kind: 'body'; range: Range };

  function findAllMatches(query: string): Match[] {
    if (!query) return [];
    const q = query.toLowerCase();
    const out: Match[] = [];

    // Title matches come first in the ordered sequence.
    if (title) {
      const t = title.toLowerCase();
      let i = t.indexOf(q);
      while (i !== -1) {
        out.push({ kind: 'title', start: i, end: i + q.length });
        i = t.indexOf(q, i + q.length);
      }
    }

    // Then body matches in DOM order.
    if (editorEl) {
      for (const tn of gatherTextNodes(editorEl)) {
        const txt = tn.data.toLowerCase();
        let i = txt.indexOf(q);
        while (i !== -1) {
          const r = document.createRange();
          r.setStart(tn, i);
          r.setEnd(tn, i + q.length);
          out.push({ kind: 'body', range: r });
          i = txt.indexOf(q, i + q.length);
        }
      }
    }

    return out;
  }

  function selectMatch(idx: number) {
    const matches = findAllMatches(findQuery);
    matchCount = matches.length;
    if (!matchCount) return;
    currentMatchIdx = ((idx % matchCount) + matchCount) % matchCount;
    const m = matches[currentMatchIdx];
    if (m.kind === 'title') {
      // Title is a real <input> — use the input selection API.
      const input = document.querySelector('.title-input') as HTMLInputElement | null;
      if (input) {
        input.focus();
        input.setSelectionRange(m.start, m.end);
      }
    } else {
      const sel = window.getSelection();
      if (!sel) return;
      sel.removeAllRanges();
      sel.addRange(m.range);
      const rect = m.range.getBoundingClientRect();
      const editorRect = editorEl.getBoundingClientRect();
      if (rect.bottom > editorRect.bottom || rect.top < editorRect.top) {
        (m.range.startContainer.parentElement || editorEl).scrollIntoView({ block: 'center' });
      }
    }
  }

  function onFindInput() {
    currentMatchIdx = 0;
    selectMatch(0);
  }

  function nextMatch() { selectMatch(currentMatchIdx + 1); }
  function prevMatch() { selectMatch(currentMatchIdx - 1); }

  function replaceCurrent() {
    if (!findQuery) return;
    const matches = findAllMatches(findQuery);
    if (!matches.length) return;
    const idx = Math.min(currentMatchIdx, matches.length - 1);
    const m = matches[idx];
    if (m.kind === 'title') {
      title = title.slice(0, m.start) + replaceQuery + title.slice(m.end);
    } else {
      m.range.deleteContents();
      if (replaceQuery) m.range.insertNode(document.createTextNode(replaceQuery));
    }
    onInput();
    setTimeout(() => selectMatch(idx), 0);
  }

  function replaceAll() {
    if (!findQuery) return;
    const q = findQuery.toLowerCase();
    let count = 0;

    // Replace in title first.
    if (title) {
      const orig = title;
      const lower = orig.toLowerCase();
      if (lower.includes(q)) {
        let out = '';
        let i = 0;
        while (i < orig.length) {
          const j = lower.indexOf(q, i);
          if (j === -1) { out += orig.slice(i); break; }
          out += orig.slice(i, j) + replaceQuery;
          i = j + q.length;
          count++;
        }
        title = out;
      }
    }

    // Then walk the body text nodes.
    if (editorEl) {
      for (const tn of gatherTextNodes(editorEl)) {
        const orig = tn.data;
        const lower = orig.toLowerCase();
        if (!lower.includes(q)) continue;
        let out = '';
        let i = 0;
        while (i < orig.length) {
          const j = lower.indexOf(q, i);
          if (j === -1) { out += orig.slice(i); break; }
          out += orig.slice(i, j) + replaceQuery;
          i = j + q.length;
          count++;
        }
        tn.data = out;
      }
    }

    if (count > 0) onInput();
    matchCount = 0;
  }

  function openFind() {
    showFind = true;
    setTimeout(() => findInputEl?.focus(), 0);
  }
  function closeFind() {
    showFind = false;
    showReplace = false;
    findQuery = '';
    replaceQuery = '';
    matchCount = 0;
    window.getSelection()?.removeAllRanges();
  }

  function onFindKey(e: KeyboardEvent) {
    if (e.key === 'Escape') { closeFind(); return; }
    if (e.key === 'Enter') {
      e.preventDefault();
      if (e.shiftKey) prevMatch(); else nextMatch();
    }
  }

  // Cmd+F / Ctrl+F to open find bar — only when the editor is in scope.
  function onWindowKey(e: KeyboardEvent) {
    const cmd = e.metaKey || e.ctrlKey;
    if (cmd && e.key.toLowerCase() === 'f' && $selectedNote) {
      e.preventDefault();
      openFind();
    }
  }

  onMount(() => window.addEventListener('keydown', onWindowKey));
  onDestroy(() => window.removeEventListener('keydown', onWindowKey));
</script>

{#if $selectedNote}
  <div class="editor-pane">
    <div class="editor-toolbar">
      <div class="toolbar-left">
        {#if $isSaving}
          <span class="save-status saving">Saving...</span>
        {:else if $selectedNote?.uuid?.startsWith('tmp:')}
          <!-- tmp: UUID means the note exists only client-side; no save has
               reached Gmail yet. Distinguish from "Saved" so the user knows
               typing is required to persist. -->
          <span class="save-status draft">Draft — start typing to save</span>
        {:else}
          <span class="save-status saved">Saved</span>
        {/if}
      </div>
      <div class="toolbar-actions">
        <button class="fmt-btn" onclick={formatBold} title="Bold"><b>B</b></button>
        <button class="fmt-btn" onclick={formatItalic} title="Italic"><i>I</i></button>
        <button class="fmt-btn" onclick={formatList} title="Bullet List">• —</button>
        <button class="fmt-btn" onclick={formatTask} title="Task / Checklist">☐</button>
        <div class="divider"></div>
        <button class="delete-btn" onclick={deleteNote} title="Delete note">
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M2 3.5h10M5.5 3.5V2.5h3v1M3 3.5l.7 8h6.6l.7-8" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
        </button>
      </div>
    </div>

    {#if showFind}
      <div class="find-bar">
        <div class="find-row">
          <svg width="12" height="12" viewBox="0 0 16 16" fill="none" style="color: #888">
            <circle cx="7" cy="7" r="5" stroke="currentColor" stroke-width="1.5"/>
            <path d="M11 11l3 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
          <input
            bind:this={findInputEl}
            bind:value={findQuery}
            oninput={onFindInput}
            onkeydown={onFindKey}
            placeholder="Find in note"
            class="find-input"
          />
          <span class="find-count">
            {#if findQuery && matchCount > 0}{currentMatchIdx + 1} / {matchCount}{:else if findQuery}no match{/if}
          </span>
          <button class="find-btn" onclick={prevMatch} disabled={!matchCount} title="Previous (Shift+Enter)">‹</button>
          <button class="find-btn" onclick={nextMatch} disabled={!matchCount} title="Next (Enter)">›</button>
          <label class="find-toggle" title="Show replace">
            <input type="checkbox" bind:checked={showReplace} /> Replace
          </label>
          <button class="find-close" onclick={closeFind} title="Close (Esc)">✕</button>
        </div>
        {#if showReplace}
          <div class="find-row">
            <span style="width: 12px"></span>
            <input
              bind:value={replaceQuery}
              placeholder="Replace with"
              onkeydown={onFindKey}
              class="find-input"
            />
            <button class="find-btn-text" onclick={replaceCurrent} disabled={!matchCount}>Replace</button>
            <button class="find-btn-text" onclick={replaceAll} disabled={!matchCount}>All</button>
          </div>
        {/if}
      </div>
    {/if}

    <div class="note-date" title="Last updated">{formatNoteDate($selectedNote.date)}</div>

    <input
      class="title-input"
      bind:value={title}
      onkeydown={onTitleKeydown}
      oninput={onInput}
      placeholder="Title"
    />

    <div
      class="editor-body"
      contenteditable="true"
      bind:this={editorEl}
      oninput={onInput}
      onclick={onEditorClick}
      data-placeholder="Start writing..."
    ></div>

    {#if $error}
      <div class="error-bar">{$error}</div>
    {/if}
  </div>
{:else}
  <div class="empty-editor">
    <p>Select a note or create a new one</p>
  </div>
{/if}

<style>
  .editor-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    background: #fffef9;
    height: 100vh;
    overflow: hidden;
  }

  .editor-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 20px;
    border-bottom: 1px solid #eee;
    background: #fffef9;
  }

  .save-status {
    font-size: 11px;
  }

  .save-status.saving { color: #aaa; }
  .save-status.saved { color: #bbb; }
  .save-status.draft { color: #c97c1f; font-style: italic; }

  .toolbar-actions {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .fmt-btn {
    padding: 4px 8px;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 13px;
    color: #666;
    transition: background 0.15s;
  }

  .fmt-btn:hover {
    background: rgba(0,0,0,0.07);
  }

  .divider {
    width: 1px;
    height: 16px;
    background: #ddd;
    margin: 0 4px;
  }

  .delete-btn {
    padding: 4px 6px;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    color: #bbb;
    display: flex;
    align-items: center;
    transition: color 0.15s, background 0.15s;
  }

  .delete-btn:hover {
    color: #e55;
    background: rgba(220,50,50,0.07);
  }

  .find-bar {
    background: #fafafa;
    border-bottom: 1px solid #e8e2d4;
    padding: 6px 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .find-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .find-input {
    flex: 1;
    background: white;
    border: 1px solid #ddd;
    border-radius: 4px;
    padding: 4px 8px;
    font-size: 12px;
    font-family: inherit;
    outline: none;
  }
  .find-input:focus { border-color: #999; }
  .find-count {
    font-size: 11px;
    color: #888;
    min-width: 56px;
    text-align: right;
  }
  .find-btn {
    width: 22px;
    height: 22px;
    background: white;
    border: 1px solid #ddd;
    border-radius: 4px;
    color: #555;
    cursor: pointer;
    font-size: 13px;
    line-height: 1;
  }
  .find-btn:hover:not(:disabled) { background: #f0f0f0; }
  .find-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .find-btn-text {
    padding: 3px 10px;
    background: white;
    border: 1px solid #ddd;
    border-radius: 4px;
    font-size: 11px;
    color: #333;
    cursor: pointer;
    font-family: inherit;
  }
  .find-btn-text:hover:not(:disabled) { background: #f0f0f0; }
  .find-btn-text:disabled { opacity: 0.4; cursor: not-allowed; }
  .find-toggle {
    font-size: 11px;
    color: #555;
    display: flex;
    align-items: center;
    gap: 3px;
    cursor: pointer;
    user-select: none;
  }
  .find-close {
    background: none;
    border: none;
    cursor: pointer;
    color: #888;
    font-size: 12px;
    padding: 0 4px;
  }
  .find-close:hover { color: #333; }

  .note-date {
    font-size: 12px;
    color: #999;
    text-align: center;
    padding: 18px 28px 0;
    user-select: none;
  }

  .title-input {
    font-size: 22px;
    font-weight: 700;
    color: #1a1a1a;
    border: none;
    outline: none;
    padding: 8px 28px 8px;
    background: transparent;
    width: 100%;
    font-family: inherit;
  }

  .title-input::placeholder {
    color: #ccc;
  }

  .editor-body {
    flex: 1;
    padding: 8px 28px 28px;
    font-size: 14px;
    line-height: 1.7;
    color: #333;
    outline: none;
    overflow-y: auto;
    font-family: inherit;
  }

  .editor-body:empty::before {
    content: attr(data-placeholder);
    color: #ccc;
    pointer-events: none;
  }

  .empty-editor {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #ccc;
    font-size: 14px;
    background: #fffef9;
  }

  .error-bar {
    background: #fee;
    color: #c33;
    padding: 8px 20px;
    font-size: 12px;
    border-top: 1px solid #fcc;
  }
</style>
