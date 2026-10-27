<script lang="ts">
  import SourceList from './SourceList.svelte';
  import ConnectionsDisclosure from './ConnectionsDisclosure.svelte';
  import { bodyContent } from '../noteHtml';
  import { deletingNotes, deletionEpoch } from '../deleteNote';
  import { noteKey, sameNote, canonicalNote } from '../noteIdentity';
  import { rekeyNoteStores, recordLocalSave, refreshNotePersistence, persistenceByNote, persistenceLabel, syncBlock, savingNoteKeys, pendingLocalEdits, acknowledgeLocalEdit, blockedReasonText } from '../notePersistence';
  import { invoke } from '@tauri-apps/api/core';
  import { onMount, onDestroy, tick } from 'svelte';
  import { get } from 'svelte/store';
  import { selectedNote, notes, isSaving, error, currentAccount, markRecentlySaved, indexUpsertOnSave, indexRemoveOnDelete, tagsByAccount, setNoteTags, selectedFolder, navigateToFolder, searchQuery, activeAccounts, accountDisplay, backendLabel, selectedSmartFolder, smartFolderNotes, toggleSelectedTag, clearSelectedTags, capabilitiesByAccount, canWrite, lastPushConfirmation, folderSuggestions, folderSuggestionKey, clearFolderSuggestion, unreviewedTrust, unreviewedKey, selectedTags, newNoteFn, currentAccountCanWrite, noteTagsByAccount, tagMatchMode, tagScope, type Trust } from '../stores/notes';
  import { markReviewed, isAgentWorkspace, relativeAgo } from '../unreviewed';
  import { navigateToPane, type Pane } from '../stores/phoneNav';
  import { currentListTitle, listTitle, listNoteCount, listLoading, accountPrefixOf } from '../stores/listView';
  import { autoOpenPending } from '../controllers/selection';
  import { claimEditorSaveState, shortSaveState, claimEditorExternalChange, EXTERNAL_CHANGE_TEXT, EXTERNAL_CHANGE_TITLE, RELOAD_TITLE } from '../stores/editorSaveState';
  import FormatBar, { FORMAT_COMMANDS, FORMAT_BAR_HEIGHT, type FormatCommand } from './FormatBar.svelte';
  import HashtagPicker from './HashtagPicker.svelte';
  import { hashtagQueryAtCaret, rankTags, hashtagVocabulary, pickerKeyAction, enterPicks, caretTextContext } from '../editor/hashtagTrigger';
  import { caretPanelPosition, VIEWPORT_MARGIN } from '../editor/anchorPosition';
  import Modal from './Modal.svelte';
  import { refreshSelection } from '../selectionRefresh';
  import { moveNoteOptimistic } from '../moveNotes';
  import { viewPath } from '../viewName';
  import { contextNotice } from '../editorContextNotice';
  import type { Note } from '../types';
  import { tryExitSpecialBlock } from '../editor/exitSpecialBlock';
  import { applyTriggerAtCaret, revertTrigger, continueChecklist, exitChecklist, ensureCheckboxesNotEditable, type AppliedTrigger } from '../editor/markdownTriggers';
  import { appendTagToBody, replaceRanges, findTextRanges, hashMarkMatches, insertChecklist } from '../editor/bodyEdits';
  import { normalizeTag, tagsInEditor, tagsEditable, tagModelOf } from '../editor/bodyTags';
  import { compileFind, findBodyMatches, expandReplacement, type LineMatch } from '../editor/findMatches';
  import { tryBackspaceMergeEmptyPrevious } from '../editor/backspaceMergeEmpty';
  import { pastePlainText } from '../editor/pastePlainText';
  import { copyText } from '../clipboard';
  import { shouldRenderExternalBody, nextExternalChangeBanner, pushedBodyAfterConfirmation, isVersionRefusal, conflictCopyTitle, sameInSubstance, bannerDiagnostic, appendedSuffix } from '../editorRenderDecision';
  import Icon from './Icon.svelte';
  import EditorMetaLine from './EditorMetaLine.svelte';
  import TagsPopover from './TagsPopover.svelte';
  import Popover from './Popover.svelte';
  import { overflowWithMore } from '../editor/toolbarOverflow';
  import { pickMetaStatus, type MetaStatus } from '../editor/metaStatus';
  import NoteMoreMenu from './NoteMoreMenu.svelte';
  import { rankTagSuggestions } from '../editor/tagSuggestions';
  import { claimNoteActionsMenu } from '../stores/noteActionsMenu';
  import { registerTagEditReceiver, handOffTagEdits, type TagEdit, type TagEditReceiver } from '../editor/tagEditHandoff';
  import { startsWithOrphanCombiningMark, displayTitle } from '../textDisplay';
  import { isMacos, isAndroid } from '../stores/platform';
  import { androidLayoutMode, keyboardInset } from '../stores/viewport';
  import { isShortcutMod, shortcutModLabel } from '../editor/shortcutKeys';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { buildLinkText, scanLinks, spanToRange, INLINE_TAGS, type LinkSpan, type LinkText } from '../links/scan';
  import { targetFromHref, targetFromSpan, schemeOf, namedApp, type LinkTarget } from '../links/target';
  import { primaryOpen, type LinkContext } from '../links/handlers';
  import { createLinkPainter, hitTest } from '../links/paint';
  import LinkChip from './LinkChip.svelte';
  import { syncBlocks, pinBlockReason, retryPinBlock } from '../syncBlocks';

  let title = '';

  // ─── Tags ───────────────────────────────────────────────────────────────
  // Tags derive from the note BODY (inline #hashtags) — the round-trips-to-
  // Apple source of truth. bodyTags is re-parsed from the editor on note load
  // and on every input; the metadata line's #N and the tags popover
  // (TagsPopover, spec 2026-10-20 §6.2) reflect it. Adding a tag injects a
  // #hashtag into the body; removing one deletes its `#`. The client
  // normalization (normalizeTag) mirrors the backend's normalize_tag exactly.
  let bodyTags: string[] = [];
  // Does the account this note belongs to accept note edits? Title, body and
  // the inline #hashtags are all `Write::Notes` server-side (tags
  // here inject into the BODY and ride the normal content save — no separate
  // sidecar write, see `addNormalizedTag`/`removeTag` below), so one gate
  // covers the whole editor. `save_note` refuses independently if this ever
  // drifts. Optimistic `true` while capabilities load; see `canWrite`.
  $: editorAccountId = $selectedNote?.account_id || $currentAccount;
  // The context notice and the save label resolve the SELECTED note's
  // account specifically (not editorAccountId, which falls back to
  // $currentAccount for a brand-new note with no account_id yet) so they name
  // the account the open note actually belongs to. The metadata line's
  // account prefix (accountPrefixOf, in `meta` below) names the backend too:
  // Account.id is `{backend}:{email}`, so two accounts can share an address.
  $: ctxAccountInfo = $activeAccounts.find((a) => a.id === $selectedNote?.account_id);
  // The note is not in the view the list shows: say where each is, in words
  // (editorContextNotice.ts).
  $: contextNoticeText = $selectedNote
    ? contextNotice({
        note: $selectedNote,
        folder: $selectedFolder,
        smart: $selectedSmartFolder,
        search: $searchQuery,
        tags: $selectedTags,
        tagMatchMode: $tagMatchMode,
        tagScope: $tagScope,
        noteTags: $noteTagsByAccount,
        currentAccount: $currentAccount,
        viewTitle: $listTitle,
        noteAccountName: accountDisplay(ctxAccountInfo, $selectedNote.account_id ?? ''),
      })
    : null;
  // Names the view — never the raw `$selectedSmartFolder` object, which
  // rendered as "[object Object]".
  $: emptyContextName = viewPath(
    $selectedSmartFolder,
    $selectedFolder,
    accountDisplay($activeAccounts.find((a) => a.id === $currentAccount), $currentAccount ?? ''),
  );
  // The empty pane (spec 2026-10-20 §5.5, desktop and tablet). It says "no
  // notes" only when the mounted list has REPORTED an empty, settled listing:
  // with no list (count null), a list still loading, or a pick waiting for its
  // listing to open the first note, it stays the neutral "Select a note".
  $: emptyKind = $autoOpenPending || $listLoading !== false || $listNoteCount !== 0 ? 'select'
    : $searchQuery.trim() ? 'search'
    : $selectedSmartFolder || $selectedTags.size > 0 ? 'view'
    : 'folder';
  $: emptyAccount = $activeAccounts.find((a) => a.id === $currentAccount);
  // Unknown (accounts not loaded, or an account without a backend kind): say
  // nothing about where notes sync rather than guess.
  $: emptyBackendLabel = emptyAccount?.backend_kind ? backendLabel(emptyAccount.backend_kind) : '';
  function newNoteFromEmpty() {
    get(newNoteFn)();
  }

  $: persistenceKey = $selectedNote ? noteKey($selectedNote) : '';
  let persistenceReadKey = '';
  $: {
    const key = `${persistenceKey}:${$selectedNote?.local_version}`;
    if (key !== persistenceReadKey) {
      persistenceReadKey = key;
      if ($selectedNote) void refreshNotePersistence($selectedNote);
    }
  }
  $: saveLabel = $selectedNote ? persistenceLabel($selectedNote, $persistenceByNote[persistenceKey], ctxAccountInfo?.backend_kind === 'local_fs') : '';
  onMount(() => {
    const timer = setInterval(() => { const n = get(selectedNote); if (n) void refreshNotePersistence(n); }, 2500);
    return () => clearInterval(timer);
  });

  // ─── Folder suggestion chip (extract filing) ─────────────────────────
  // Gotcha #28: the open note's identity and the proposal compared against
  // it are read in ONE reactive expression, so switching notes can never
  // show the previous note's proposal for a pass. The chip also hides once
  // the note already sits in the proposed folder — which is also what makes
  // an optimistic move hide it, and a rollback bring it back.
  $: chipSuggestion = (() => {
    const n = $selectedNote;
    if (!n?.uuid || !n.account_id) return null;
    const s = $folderSuggestions[folderSuggestionKey(n.account_id, n.uuid)];
    if (!s || s.path === n.label) return null;
    return s;
  })();
  // ─── Trust chip (note provenance) ────────────────────────────────────
  // Same shape as chipSuggestion: identity and the entry compared against it
  // are read in ONE expression (gotcha #28). The agent workspace is never
  // flagged. The store is refreshed by App.loadNotes on every remote-changed.
  $: trustChip = (() => {
    const n = $selectedNote;
    if (!n?.uuid || !n.account_id || isAgentWorkspace(n.label ?? '')) return null;
    return $unreviewedTrust[unreviewedKey(n.account_id, n.uuid)]?.trust ?? null;
  })();
  let trustError = '';
  // Mark reviewed certifies what is ON SCREEN (spec §5.2). The chip's trust is
  // read from SQLite, but the open note's body reaches the store by another
  // route — the full listing, which waits on the remote — so the chip can name
  // a change the editor has not rendered: a stale verify puts the new writer
  // back at once, and refreshUnreviewed can land after a listing that predates
  // the change. Found live 2026-10-08: ~48 s in which a second click certified
  // an agent's line the user never saw.
  //
  // So whenever the chip's trust for the open note changes, re-read the note
  // from SQLite and hand it to refreshSelection — reconcileSelection's route,
  // so the render decision below renders it, or holds it back behind the
  // "Edited on another device" banner while there is unsaved work. Until that
  // read lands, and while a change is held back, the button is disabled. Key
  // and flag are set in the pass that changes the chip (gotcha #28).
  //
  // The selection's own body and version are in the key too: a listing fetched
  // before the change can land AFTER this read and put the old body back
  // (PR #139 review). Any body change under a shown chip therefore re-reads
  // SQLite before the button comes back; a read that changes nothing settles.
  let reviewReadKey = '';
  let reviewReadPending = false;
  const READ_FAILED = 'Could not load the latest version.';
  $: {
    const n = $selectedNote;
    const k = n && trustChip
      ? `${noteKey(n)}:${trustChip.event_id}:${trustChip.note_local_version}:${n.local_version}:${n.body_html}`
      : '';
    if (k !== reviewReadKey) {
      reviewReadKey = k;
      reviewReadPending = !!k;
      if (n && k) void rereadForReview(n, k);
    }
  }
  async function rereadForReview(n: Note, k: string) {
    let fresh: Note | null;
    try {
      fresh = await invoke<Note | null>('get_cached_note', { accountId: n.account_id, uuid: n.uuid });
    } catch (e) {
      console.error('get_cached_note failed', e);
      // Stays disabled: nothing shows the body on screen is current. Opening
      // the note again retries.
      if (k === reviewReadKey) trustError = READ_FAILED;
      return;
    }
    if (k !== reviewReadKey) return; // another note, or a newer chip, has its own read
    // Same synchronous turn as the selection update: the flush that clears the
    // flag also runs the render decision, so a held-back change raises the
    // banner in that same pass and the button never shows enabled between.
    // A pre-rekey uuid read through its alias (gotcha #16): forward the
    // selection first, or refreshSelection would not recognise the note.
    if (fresh && n.account_id && fresh.uuid !== n.uuid) rekeyNoteStores(n.account_id, n.uuid, fresh.uuid);
    if (fresh) refreshSelection(fresh);
    if (trustError === READ_FAILED) trustError = '';
    reviewReadPending = false;
  }
  $: reviewBlocked = reviewReadPending || externalChangePending;
  // ⋯'s Mark reviewed (ruling R25: offered whenever trust is pending, even
  // while a higher-priority status holds the indicator) runs the SAME handler
  // as the status popover's button. Its error line lives in that popover,
  // which is closed when the user acted from ⋯, so a failure also goes to the
  // global error bar — never silent.
  async function markReviewedFromMenu() {
    await onMarkReviewed();
    if (trustError) error.set(trustError);
  }
  async function onMarkReviewed() {
    const n = $selectedNote;
    if (!n?.uuid || !n.account_id || reviewBlocked) return;
    trustError = '';
    // In the Unreviewed view the row leaves together with the chip, before the
    // IPC (local-first doctrine, spec §5.2), and comes back at its old index if
    // the review is refused as stale or fails.
    let removed: { index: number; note: Note } | null = null;
    if ($selectedSmartFolder?.kind === 'unreviewed') {
      const list = get(smartFolderNotes);
      const index = list.findIndex((x) => sameNote(x, n));
      if (index >= 0) {
        removed = { index, note: list[index] };
        smartFolderNotes.set([...list.slice(0, index), ...list.slice(index + 1)]);
      }
    }
    // The version the text on screen derives from, not the chip's: if the UI
    // ever races, Db::verify_note still refuses an event newer than it.
    const r = await markReviewed(n.account_id, n.uuid, saveBaseVersions.get(noteKey(n)));
    if (r === 'failed') trustError = 'Could not mark reviewed.';
    if (r !== 'verified' && removed) {
      const back = removed;
      smartFolderNotes.update((l) =>
        l.some((x) => sameNote(x, back.note)) ? l : [...l.slice(0, back.index), back.note, ...l.slice(back.index)],
      );
    }
  }
  // R14: name who wrote what is unreviewed (`unreviewed_by`), not the latest
  // writer (`by`) — a typo the user fixed in an Extract note must not read as
  // the user's.
  function trustLabel(t: Trust): string {
    if (t.unrecorded_change) return 'changed without a record';
    if (!t.unreviewed_by) return 'changed elsewhere';
    const ago = t.unreviewed_at ? relativeAgo(t.unreviewed_at) : '';
    return `${t.unreviewed_by.split('/')[0]}${ago ? ` · ${ago}` : ''}`;
  }
  // The full sentence the metadata line's status popover shows for the
  // indicator's trustLabel (spec 2026-10-20 §6.3).
  function trustSentence(t: Trust): string {
    if (t.unrecorded_change) return 'Changed without a record of who changed it.';
    if (!t.unreviewed_by) return 'Changed elsewhere and not reviewed yet.';
    const ago = t.unreviewed_at ? relativeAgo(t.unreviewed_at) : '';
    return `Changed by ${t.unreviewed_by.split('/')[0]}${ago ? ` ${ago}` : ''} and not reviewed yet.`;
  }
  $: canRelocate = canWrite(
    editorAccountId ? $capabilitiesByAccount[editorAccountId] : undefined,
    'relocate',
  );

  // A SNAPSHOT of the account's folder list, taken once when the chip
  // appears — not re-read at click time, because re-reading would await IPC
  // before the optimistic write (local-first doctrine). It catches a proposal
  // naming a folder that was already gone when the chip appeared. A folder
  // deleted AFTER that is not seen here; it is left to `move_notes_batch`.
  // `null` = not loaded yet: the move proceeds on the same terms.
  let chipFolderPaths: Set<string> | null = null;
  let chipFolderKey = '';
  $: {
    const acct = $selectedNote?.account_id;
    const key = chipSuggestion && acct ? `${acct}:${$selectedNote?.uuid}:${chipSuggestion.path}` : '';
    if (key !== chipFolderKey) {
      chipFolderKey = key;
      chipFolderPaths = null;
      if (key && acct) {
        invoke<string[]>('list_folders', { accountId: acct })
          .then((paths) => {
            if (chipFolderKey === key) chipFolderPaths = new Set(paths);
          })
          .catch((e) => console.warn('folder list for the suggestion chip failed', e));
      }
    }
  }

  async function acceptFolderSuggestion() {
    const n = $selectedNote;
    const s = chipSuggestion;
    if (!n?.account_id || !s) return;
    const accountId = n.account_id;
    const uuid = n.uuid;
    if (chipFolderPaths && !chipFolderPaths.has(s.path)) {
      clearFolderSuggestion(accountId, uuid);
      error.set(`The folder "${s.path}" no longer exists.`);
      return;
    }
    const result = await moveNoteOptimistic({
      aiResultId: s.ai_result_id ?? '',
      accountId,
      uuid,
      fromLabel: n.label,
      toLabel: s.path,
      remoteId: n.id,
    });
    if (result.ok) {
      clearFolderSuggestion(accountId, uuid);
    } else {
      // Rolled back by the helper; the proposal stays so the user can retry.
      console.error('move to suggested folder failed', result.error);
      error.set(`Failed to move note: ${result.error}`);
    }
  }

  function dismissFolderSuggestion() {
    const n = $selectedNote;
    if (n?.account_id) clearFolderSuggestion(n.account_id, n.uuid);
  }

  $: canWriteNotes = canWrite(
    editorAccountId ? $capabilitiesByAccount[editorAccountId] : undefined,
    'notes',
  );
  // The account may write notes, but THIS note's last push was refused by the
  // remote's own document (iCloud per-note writability, gotcha #24): no edit
  // made here can get past that, so the note is locked rather than inviting
  // edits that are each refused in turn. Other blocks stay editable — an edit
  // or a move may be their fix, and it re-arms the push (gotcha #14).
  $: editable = canWriteNotes && !$selectedNote?.push_blocked_by_remote;

  $: currentTags = $selectedNote ? bodyTags : [];

  // What a tag is on this account (gotcha #38): a `#word` in the text, or —
  // on iCloud — only a hashtag span, which Jodd cannot create. There the tags
  // popover is display-only: adding would write `#tag` TEXT, which Apple never
  // treats as a tag, and a real tag's note is not writable anyway. Known on
  // the first render: `tagModelOf` falls back to the account id's backend
  // while capabilities load, so nothing re-reads the tags when they arrive.
  $: editorTagModel = tagModelOf(editorAccountId ? $capabilitiesByAccount[editorAccountId] : undefined, editorAccountId);
  $: canEditTags = editable && tagsEditable(editorTagModel);
  // The popover's Learn more, where tags are read-only because of what a tag
  // IS on this account — not where only this note is locked. A note not yet
  // saved (`tmp:`) gets neither line: it takes tags once its first save lands.
  $: tagsReadOnlyReason = tagsEditable(editorTagModel)
    ? null
    : "Apple Notes stores tags as special objects Jodd can't create yet.";

  // Autocomplete: existing tags for this account that match what's typed and
  // aren't already on the note. Empty input → all available (focus to
  // browse). A new function whenever the account's tags or the note's change,
  // so the popover's list recomputes with them.
  $: tagSuggestions = (() => {
    const accountId = $selectedNote ? $selectedNote.account_id ?? $currentAccount : null;
    const own = new Set(bodyTags);
    const all = accountId
      ? ($tagsByAccount.get(accountId) ?? []).map((t) => t.tag).filter((t) => !own.has(t))
      : [];
    return (typed: string): string[] => {
      const q = normalizeTag(typed);
      // Prefix matches first (rankTagSuggestions).
      return rankTagSuggestions(all, q, 8);
    };
  })();

  function refreshBodyTags() {
    // The model is read here, from the note being read, not from a reactive
    // copy: the load path calls this on a note switch, and a `$:` value could
    // still hold the previous note's account in that turn (gotcha #28).
    const shown = get(selectedNote);
    const shownAccount = shown?.account_id || get(currentAccount) || null;
    const model = tagModelOf(shownAccount ? get(capabilitiesByAccount)[shownAccount] : undefined, shownAccount);
    const next = editorEl ? tagsInEditor(editorEl, model) : [];
    const changed =
      next.length !== bodyTags.length || next.some((t, i) => t !== bodyTags[i]);
    bodyTags = next;
    // Keep the shared tag store in sync with the body LIVE — that store derives
    // tagsByAccount, which drives the sidebar Tags filter + note-list filtering.
    // Without this the sidebar only picked up new tags on the next app load.
    if (changed) {
      const note = get(selectedNote);
      if (note && !note.uuid.startsWith('tmp:')) {
        const accountId = note.account_id ?? get(currentAccount);
        if (accountId) setNoteTags(accountId, note.uuid, next);
      }
    }
  }

  // The add path for the tags popover — typing-then-Enter and picking a
  // suggestion alike.
  // Injects the hashtag into the BODY (source of truth) and lets the normal
  // content save derive + round-trip it; no separate sidecar write.
  function addNormalizedTag(tag: string) {
    const note = $selectedNote;
    if (!note || !tag || !editorEl || !canEditTags) return;
    if (note.uuid.startsWith('tmp:')) return; // hidden for tmp: notes anyway
    if (bodyTags.includes(tag)) return;
    // As an edit command, so Cmd+Z can take it back (../editor/bodyEdits.ts).
    appendTagToBody(editorEl, tag);
    refreshBodyTags();
    onInput(); // optimistic store update + schedule the content save
  }

  // Un-tag every occurrence in the body (inline + the trailing tag line) by
  // deleting its `#` and keeping the word — Apple's own Delete Tag keeps its
  // text too — then save. hashMarkMatches keeps #work from also matching
  // #working; the deletes are edit commands so Cmd+Z restores the tag.
  function removeTag(tag: string) {
    const note = $selectedNote;
    if (!note || !editorEl || !canEditTags) return;
    replaceRanges(editorEl, findTextRanges(editorEl, hashMarkMatches(tag)), '');
    refreshBodyTags();
    onInput();
  }
  // Backlinks / outgoing links ([[wikilink]] graph) for the open note. Fetched
  // from the edges table on load + after each save; rendered in a footer panel.
  let connections: { outgoing: Note[]; backlinks: Note[] } = { outgoing: [], backlinks: [] };
  let connSeq = 0;
  async function refreshConnections() {
    const note = get(selectedNote);
    if (!note || note.uuid.startsWith('tmp:')) {
      connections = { outgoing: [], backlinks: [] };
      return;
    }
    const accountId = note.account_id ?? get(currentAccount);
    if (!accountId) return;
    const seq = ++connSeq;
    try {
      const res = await invoke<{ outgoing: Note[]; backlinks: Note[] }>('note_connections', {
        accountId,
        uuid: note.uuid,
      });
      if (seq === connSeq && sameNote(get(selectedNote), note)) connections = res;
    } catch (e) {
      if (seq === connSeq && sameNote(get(selectedNote), note)) connections = { outgoing: [], backlinks: [] };
    }
  }

  // URLs this note cites (`cites` edges). Fetched alongside connections;
  // sequence-guarded the same way to drop stale responses.
  let citations: string[] = [];
  let citeSeq = 0;
  let citationsLoading = false;
  let citationsFailed = false;
  // True once this note's citations have been read at least once. A re-read
  // after every save of a note with no sources must not flash "Sources…" in the
  // connections line (it would appear and vanish under the caret's pane).
  let citationsSettled = false;
  async function refreshCitations() {
    const note = get(selectedNote);
    const seq = ++citeSeq;
    citationsFailed = false;
    citationsLoading = false;
    if (!note || note.uuid.startsWith('tmp:')) {
      citations = [];
      return;
    }
    const accountId = note.account_id ?? get(currentAccount);
    if (!accountId) return;
    citationsLoading = true;
    try {
      const res = await invoke<string[]>('note_citations', { accountId, uuid: note.uuid });
      if (seq === citeSeq && sameNote(get(selectedNote), note)) { citations = res; citationsSettled = true; }
    } catch (e) {
      // Nothing is known about this note's sources any more: a Retry must
      // say "Sources…" while it reads, not hide the line as a settled empty
      // list would.
      if (seq === citeSeq && sameNote(get(selectedNote), note)) { citations = []; citationsFailed = true; citationsSettled = false; }
    } finally {
      if (seq === citeSeq && sameNote(get(selectedNote), note)) citationsLoading = false;
    }
  }
  function openConnection(n: Note) {
    if (n.account_id && n.account_id !== get(currentAccount)) currentAccount.set(n.account_id);
    // The linked note may live in a folder that isn't the one currently
    // loaded into $notes. Without upserting it here, the next background
    // reconciliation (sweep tick / folder settle / poll) fetches only the
    // loaded scope, can't find this uuid, and App.svelte's reconcileSelection
    // treats that as "deleted externally" and clears the selection — the note
    // opens for a moment and then reverts to the empty state.
    notes.update((ns) => {
      const i = ns.findIndex((x) => sameNote(x, n));
      if (i >= 0) ns[i] = n;
      else ns.push(n);
      return ns;
    });
    selectedNote.set(n);
  }

  // `[[` link picker. Typing `[[query` opens an autocomplete of notes; picking
  // one inserts `[[<slug>]]` (slug = title + stable uuid id) so the link is
  // unambiguous even across duplicate titles.
  type LinkCandidate = { uuid: string; title: string; label: string; slug: string };
  let linkPickerOpen = false;
  let linkQuery = '';
  let linkResults: LinkCandidate[] = [];
  let linkIndex = 0;
  // The picker's inline position: at the caret (desktop, tablet), or — on a
  // phone — kept clear of the format bar and the keyboard (linkPickerStyle).
  let linkStyle = '';
  let linkAnchor: { node: Text; start: number; end: number } | null = null;
  let linkSeq = 0;

  function closeLinkPicker() {
    linkPickerOpen = false;
    linkAnchor = null;
    linkResults = [];
  }

  // Detect `[[query` immediately before the caret (no closing ]] yet).
  function detectLinkPicker() {
    const sel = typeof window !== 'undefined' ? window.getSelection() : null;
    if (!sel || sel.rangeCount === 0 || !editorEl) return closeLinkPicker();
    const range = sel.getRangeAt(0);
    const node = range.startContainer;
    if (!range.collapsed || node.nodeType !== 3 || !editorEl.contains(node)) return closeLinkPicker();
    const before = (node as Text).data.slice(0, range.startOffset);
    const m = before.match(/\[\[([^\[\]\n]*)$/);
    if (!m) return closeLinkPicker();
    // Don't trigger when the caret is INSIDE an already-completed [[link]] (a
    // `]]` follows before any newline) — otherwise arrowing through an existing
    // link re-opens the picker and swallows the arrow keys.
    const after = (node as Text).data.slice(range.startOffset);
    if (/^[^[\n]*\]\]/.test(after)) return closeLinkPicker();
    linkQuery = m[1];
    linkAnchor = { node: node as Text, start: range.startOffset - m[0].length, end: range.startOffset };
    linkStyle = linkPickerStyle(range.getBoundingClientRect());
    linkPickerOpen = true;
    fetchLinkResults();
  }

  // Its max-height and widest width (.link-picker below).
  const LINK_PICKER_MAX_HEIGHT = 240;
  const LINK_PICKER_MAX_WIDTH = 340;
  // Desktop and tablet: below the caret, as it always was. A phone: typing at
  // the bottom of a long note puts the caret right on the format bar, and
  // below it is the bar and the keyboard — so above the caret when the list
  // would not fit below, clamped inside what is visible
  // (caretPanelPosition, shared with Task 6.3's #-autocomplete).
  function linkPickerStyle(rect: DOMRect): string {
    const p = caretListPosition(rect, LINK_PICKER_MAX_WIDTH, LINK_PICKER_MAX_HEIGHT);
    if (p.maxHeight === undefined) return `left:${p.left}px; top:${p.top}px`;
    const vertical = p.top !== undefined ? `top:${p.top}px` : `bottom:${p.bottom}px`;
    return `left:${p.left}px; ${vertical}; max-height:${p.maxHeight}px; max-width:calc(100vw - ${2 * VIEWPORT_MARGIN}px)`;
  }
  // Where a caret list goes — the `[[` picker and the # picker alike. No
  // maxHeight off a phone: the list's own CSS caps it there.
  type CaretListPosition = { left: number; top?: number; bottom?: number; maxHeight?: number };
  function caretListPosition(rect: DOMRect, width: number, maxHeight: number): CaretListPosition {
    if (!phoneLayout) return { left: rect.left || 80, top: (rect.bottom || 120) + 2 };
    const bottomInset = (formatBarShown ? FORMAT_BAR_HEIGHT : 0) + get(keyboardInset);
    return caretPanelPosition(
      { left: rect.left, top: rect.top, bottom: rect.bottom },
      { width, maxHeight },
      { width: window.innerWidth, height: window.innerHeight },
      bottomInset,
    );
  }

  async function fetchLinkResults() {
    const note = get(selectedNote);
    const accountId = note?.account_id ?? get(currentAccount);
    if (!accountId) { linkResults = []; return; }
    const seq = ++linkSeq;
    try {
      const res = await invoke<LinkCandidate[]>('search_note_links', { accountId, query: linkQuery });
      if (seq === linkSeq) {
        linkResults = res.filter((r) => r.uuid !== note?.uuid); // can't link to self
        linkIndex = 0;
      }
    } catch (e) {
      if (seq === linkSeq) linkResults = [];
    }
  }

  function pickLink(c: LinkCandidate) {
    if (!linkAnchor) return;
    const { node, start, end } = linkAnchor;
    if (!editorEl || !editorEl.contains(node) || end > node.data.length) { closeLinkPicker(); return; }
    // As an edit command, so Cmd+Z can take it back; insertText leaves the
    // caret just after the link (../editor/bodyEdits.ts).
    const range = document.createRange();
    range.setStart(node, start);
    range.setEnd(node, end);
    replaceRanges(editorEl, [range], `[[${c.slug}]]`, { giveBackFocus: false });
    closeLinkPicker();
    editorEl.focus();
    onInput(); // save + derive the edge
  }

  // #-autocomplete (spec 2026-10-20 §6.4). Typing `#` that starts a word
  // lists the account's existing tags for the prefix after it; picking one
  // replaces the typed `#prefix` with `#tag ` as an edit command, so Cmd+Z
  // brings the prefix back. Never on iCloud (gotcha #38): a `#word` there is
  // text, not a tag. When nothing matches, nothing shows — a new tag is just
  // the text typed, as before.
  let hashtagItems: string[] = [];
  let hashtagIndex = 0;
  // The typed prefix, and whether ↑/↓ moved the highlight — together they
  // decide whether Enter takes a tag (enterPicks, ruling R31).
  let hashtagQuery = '';
  let hashtagMoved = false;
  let hashtagPosition: CaretListPosition = { left: 0, top: 0 };
  // The typed `#prefix`, live: it tracks the edits around it.
  let hashtagAnchor: Range | null = null;
  // Where the `#` was when Esc closed the list: it stays closed for that `#`
  // until the caret leaves its word.
  let hashtagDismissed: { node: Node; offset: number } | null = null;
  // Ids for the caret lists' rows: the body keeps focus while a list is
  // open, so it names the active row with aria-activedescendant.
  const caretListUid = `caret-list-${Math.random().toString(36).slice(2, 8)}`;
  $: activeCaretOption = hashtagItems.length && !titleFocused
    ? `${caretListUid}-tag-${hashtagIndex}`
    : linkPickerOpen && linkResults.length
      ? `${caretListUid}-link-${linkIndex}`
      : undefined;
  const HASHTAG_PICKER_MAX_HEIGHT = 240;
  const HASHTAG_PICKER_MAX_WIDTH = 340;

  function closeHashtagPicker() {
    // Only when something is open: this runs on every input and caret key.
    if (hashtagItems.length) hashtagItems = [];
    if (hashtagAnchor) hashtagAnchor = null;
  }

  /**
   * `followOnly`: a caret move (click or caret key), not typing. It may keep
   * the open list on its own `#` — the caret moved within the typed prefix —
   * but never carry it to a different `#tag`: there the list would own Enter
   * (a prefix match picks) and take the newline the user typed.
   */
  function detectHashtagPicker(followOnly = false) {
    const sel = typeof window !== 'undefined' ? window.getSelection() : null;
    if (!sel || sel.rangeCount === 0 || !editorEl || linkPickerOpen || !editable) return closeHashtagPicker();
    // The account is read from the note being read, not a `$:` copy — the
    // same turn as the body it is checked against (gotcha #28).
    const note = get(selectedNote);
    const accountId = note?.account_id || get(currentAccount) || null;
    if (!note || !accountId) return closeHashtagPicker();
    if (!tagsEditable(tagModelOf(get(capabilitiesByAccount)[accountId], accountId))) return closeHashtagPicker();
    const range = sel.getRangeAt(0);
    const node = range.startContainer;
    if (!range.collapsed || node.nodeType !== 3 || !editorEl.contains(node)) return closeHashtagPicker();
    // The caret's line up to the caret, across inline elements and split
    // text nodes: `<span>hello</span>#st` is `hello#st`, which is no tag.
    const ctx = caretTextContext(editorEl, node as Text, range.startOffset);
    const inCodeOrLink = (n: Node) => {
      const holder = n.parentElement?.closest('pre, code, a');
      return !!holder && editorEl!.contains(holder);
    };
    const query = hashtagQueryAtCaret(ctx.before, inCodeOrLink(node));
    // A caret inside a word (`#wo|rk`) is not typing a tag: a pick would
    // leave `#work rk`. The `[[` picker's "after" check, for tags.
    if (query === null || ctx.continuesWord) {
      hashtagDismissed = null;
      return closeHashtagPicker();
    }
    // Where the `#` is, mapped back to its text node.
    const hash = ctx.locate(ctx.before.length - query.length - 1);
    if (!hash || hash.node.data[hash.offset] !== '#' || inCodeOrLink(hash.node)) return closeHashtagPicker();
    const hashNode = hash.node;
    const at = hash.offset;
    if (hashtagDismissed && hashtagDismissed.node === hashNode && hashtagDismissed.offset === at) {
      return closeHashtagPicker();
    }
    // The open list's anchor is live, so its start is where its `#` is now.
    if (followOnly && !(hashtagAnchor && hashtagAnchor.startContainer === hashNode && hashtagAnchor.startOffset === at)) {
      return closeHashtagPicker();
    }
    hashtagDismissed = null;
    const anchor = document.createRange();
    anchor.setStart(hashNode, at);
    anchor.setEnd(node, range.startOffset);
    const own = get(noteTagsByAccount).get(accountId)?.get(note.uuid) ?? [];
    const cloud = get(tagsByAccount).get(accountId) ?? [];
    const items = rankTags(query, hashtagVocabulary(cloud, own, query));
    // Keep the highlighted row when the list did not change — a caret key's
    // keyup re-runs this after ↑/↓ moved it.
    if (items.length !== hashtagItems.length || items.some((t, i) => t !== hashtagItems[i])) {
      hashtagIndex = 0;
      hashtagMoved = false;
    }
    hashtagQuery = query;
    hashtagItems = items;
    hashtagAnchor = anchor;
    if (items.length) {
      const r = anchor.getBoundingClientRect();
      hashtagPosition = caretListPosition(r, HASHTAG_PICKER_MAX_WIDTH, HASHTAG_PICKER_MAX_HEIGHT);
    }
  }

  // A caret key or a click only follows a list that is already open, on its
  // own `#` — or closes it. Landing on an existing `#work` is not typing a
  // tag; opening (or moving the list) there would take ↑/↓ and Enter away
  // from the editor.
  function refreshOpenHashtagPicker() {
    if (hashtagItems.length) detectHashtagPicker(true);
    else closeHashtagPicker();
  }

  function dismissHashtagPicker() {
    if (hashtagAnchor) hashtagDismissed = { node: hashtagAnchor.startContainer, offset: hashtagAnchor.startOffset };
    closeHashtagPicker();
  }

  function pickHashtag(tag: string) {
    const anchor = hashtagAnchor;
    closeHashtagPicker();
    if (!anchor || !editorEl || !editorEl.contains(anchor.startContainer) || !editorEl.contains(anchor.endContainer)) return;
    // A live Range follows the edits around it; if they left it no longer
    // spanning a `#prefix` (an edit the list did not see), do nothing rather
    // than overwrite the wrong text.
    if (anchor.startContainer.nodeType !== 3 || !anchor.toString().startsWith('#')) return;
    // As an edit command over a selection spanning the typed `#prefix`, so
    // Cmd+Z restores it (../editor/bodyEdits.ts). The trailing space ends the
    // tag, which closes the list again.
    replaceRanges(editorEl, [anchor], `#${tag} `, { giveBackFocus: false });
    editorEl.focus();
    onInput(); // save + derive the tag
  }

  // Re-evaluate the [[ picker after caret-moving keys so it closes once the
  // caret leaves the [[query context — otherwise it stays open and swallows the
  // arrow keys (you couldn't move the cursor).
  function onEditorKeyup(e: KeyboardEvent) {
    if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown'].includes(e.key)) {
      detectLinkPicker();
      refreshOpenHashtagPicker();
    }
  }

  function onEditorKeydown(e: KeyboardEvent) {
    // Backspace immediately after a markdown trigger reverts it. We consume
    // the Backspace (preventDefault) so the user gets the trigger text back
    // instead of also deleting a character.
    if (lastTrigger && e.key === 'Backspace' && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      const applied = lastTrigger;
      lastTrigger = null;
      revertTrigger(applied);
      return;
    }
    // Any other keystroke invalidates the pending revert — the trigger has
    // been "accepted" by continued typing.
    if (lastTrigger && e.key !== 'Shift' && e.key !== 'Meta' && e.key !== 'Control' && e.key !== 'Alt') {
      lastTrigger = null;
    }

    // WebKit's native backspace-merge loses the current block's tag when the
    // previous line is empty (a heading merging backward into a blank line
    // comes out demoted to plain text) — see backspaceMergeEmpty.ts.
    if (e.key === 'Backspace' && !e.metaKey && !e.ctrlKey && !e.altKey && tryBackspaceMergeEmptyPrevious(editorEl)) {
      e.preventDefault();
      onInput();
      return;
    }

    // Keyboard shortcuts for first-class formatting. All listed in the ⓘ
    // cheatsheet so they are not undocumented behaviour. We use e.code (the
    // physical key, e.g. 'Digit8') rather than e.key (the produced character)
    // because Shift+8 produces '*' on US layouts, Alt+1 produces '¡' on macOS,
    // and AZERTY shifts the entire digit row — all of which would silently
    // break e.key-based matching. e.code is layout- and modifier-independent.
    const mod = isShortcutMod(e, $isMacos);
    // Mod+Enter with the caret in a link opens it. It is the first branch to
    // look at Enter: the checklist branch below ignores modifiers, so a link
    // on a checklist row would otherwise add a row. Outside a link it falls
    // through untouched.
    if (mod && !e.altKey && !e.shiftKey && e.key === 'Enter' && !e.isComposing && openLinkAtCaret()) {
      e.preventDefault();
      return;
    }
    if (mod && !e.altKey) {
      // Undo/redo are handled here rather than left to the platform. On macOS,
      // Tauri's default Edit menu does carry Undo ⌘Z / Redo ⇧⌘Z — measured in
      // WKWebView (2026-09-14): this handler runs first, and its
      // preventDefault keeps the menu from undoing a second time.
      // Mod+Shift+Z is redo everywhere; Ctrl+Y only off macOS, where it is the
      // Windows redo convention (on macOS it was an undocumented Cmd+Y).
      if (!e.shiftKey && e.code === 'KeyZ') { e.preventDefault(); formatUndo(); onInput(); return; }
      if (e.shiftKey && e.code === 'KeyZ') { e.preventDefault(); formatRedo(); onInput(); return; }
      if (!$isMacos && !e.shiftKey && e.code === 'KeyY') { e.preventDefault(); formatRedo(); onInput(); return; }
      // Cmd/Ctrl+B/I/U: WebKit normally handles these via its built-in editing
      // key bindings, but in Tauri 2 on macOS Tahoe the Cocoa menu doesn't have
      // matching key equivalents (no Format menu), and the chord never reaches
      // WebKit's editing pipeline — Cmd+B silently no-ops. Intercepting in JS
      // and calling execCommand directly makes it work regardless of the menu.
      if (!e.shiftKey && e.code === 'KeyB') { e.preventDefault(); formatBold(); onInput(); return; }
      if (!e.shiftKey && e.code === 'KeyI') { e.preventDefault(); formatItalic(); onInput(); return; }
      if (!e.shiftKey && e.code === 'KeyU') { e.preventDefault(); formatUnderline(); onInput(); return; }
      if (e.shiftKey && e.code === 'KeyX') { e.preventDefault(); formatStrike(); onInput(); return; }
      if (!e.shiftKey && e.code === 'KeyK') { e.preventDefault(); formatLink(); return; }
      if (e.shiftKey && e.code === 'Digit7') { e.preventDefault(); formatOrderedList(); onInput(); return; }
      if (e.shiftKey && e.code === 'Digit8') { e.preventDefault(); formatList(); onInput(); return; }
      if (e.shiftKey && e.code === 'Digit9') { e.preventDefault(); formatTask(); return; }
      if (e.shiftKey && e.code === 'Period') { e.preventDefault(); formatBlockquote(); return; }
      if (!e.shiftKey && e.code === 'KeyE') { e.preventDefault(); formatInlineCode(); return; }
      if (e.shiftKey && e.code === 'KeyC') { e.preventDefault(); formatCodeBlock(); return; }
    }
    if (mod && e.altKey && !e.shiftKey) {
      if (e.code === 'Digit1') { e.preventDefault(); formatHeadingLevel(1); return; }
      if (e.code === 'Digit2') { e.preventDefault(); formatHeadingLevel(2); return; }
      if (e.code === 'Digit3') { e.preventDefault(); formatHeadingLevel(3); return; }
    }

    // An open caret list takes the navigation keys first (Tab = accept, like
    // Enter) — never mid-composition (pickerKeyAction's IME guard).
    if (linkPickerOpen && linkResults.length > 0) {
      const a = pickerKeyAction(e.key, linkIndex, linkResults.length, e);
      if (a) {
        e.preventDefault();
        if (a === 'pick') pickLink(linkResults[linkIndex]);
        else if (a === 'close') closeLinkPicker();
        else linkIndex = a.move;
        return;
      }
    }
    if (hashtagItems.length > 0 && !e.metaKey && !e.ctrlKey && !e.altKey) {
      const a = pickerKeyAction(e.key, hashtagIndex, hashtagItems.length, e);
      // Enter/Tab take a tag only per enterPicks (R31), never with Shift;
      // otherwise the key is the editor's (a new line, an indent).
      const picks = a === 'pick' && !e.shiftKey && enterPicks(hashtagQuery, hashtagItems, hashtagMoved);
      if (a && (a !== 'pick' || picks)) {
        e.preventDefault();
        if (a === 'pick') pickHashtag(hashtagItems[hashtagIndex]);
        else if (a === 'close') dismissHashtagPicker();
        else {
          hashtagIndex = a.move;
          hashtagMoved = true;
        }
        return;
      }
    }
    // Enter on a checklist line → continue the checklist with a new task (or
    // exit to a plain line when the task is empty), carrying the indent level.
    if (e.key === 'Enter' && !e.shiftKey) {
      const tb = taskBlock();
      if (tb) {
        e.preventDefault();
        const text = (tb.textContent || '').replace(/ /g, '').trim();
        // Through execCommand, not replaceWith/after: WebKit's undo history
        // never saw those, so Cmd+Z could not remove the row they added
        // (measured — see ../editor/markdownTriggers.ts).
        if (text === '') exitChecklist(tb);
        else continueChecklist(tb);
        onInput();
        return;
      }
      // Enter on an empty line inside a blockquote / heading → exit the block
      // to a plain paragraph below. Same UX pattern as Notion / Bear: one Enter
      // adds a new line within the special block; a second Enter (now on an
      // empty line) exits it.
      if (!e.metaKey && !e.ctrlKey && !e.altKey && tryExitSpecialBlock(editorEl)) {
        e.preventDefault();
        onInput();
        return;
      }
      // Enter at the very start of the first block is left to WebKit. It was
      // split by hand (splitFirstBlock.ts, 2026-07) for a misfire that no
      // longer reproduces — 2026-09-14, native Enter moves the content down
      // for a heading, a <div> and a bare text node, caret placed by script
      // or by keyboard — and the hand-built split could not be undone.
    }
    // Tab / Shift-Tab → outline indent / outdent.
    if (e.key === 'Tab') {
      e.preventDefault();
      indentSelection(e.shiftKey);
    }
  }

  // The caret's block if it's a checklist row (a line whose first child is a
  // checkbox), else null.
  function taskBlock(): HTMLElement | null {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0 || !editorEl) return null;
    const node = sel.getRangeAt(0).startContainer;
    const el = (node.nodeType === 3 ? node.parentElement : node) as HTMLElement | null;
    const block = topBlock(el);
    return block && block.querySelector(':scope > input[type=checkbox]') ? block : null;
  }


  function setCheckboxState(cb: HTMLInputElement, val: boolean) {
    cb.checked = val;
    if (val) cb.setAttribute('checked', '');
    else cb.removeAttribute('checked');
  }

  // Nested-checklist propagation. Indent level (margin / 28) defines the tree:
  // a task's "subtree" is the consecutive deeper rows below it. Toggling a
  // parent sets its whole subtree; then, bottom-up, every parent becomes done
  // iff all of its subtree is done (so finishing the last subtask ticks the
  // parent, unticking one unticks it).
  //
  // This editor is where the 28px step and the 168px ceiling below ORIGINATE;
  // the Rust side mirrors them as `TASK_INDENT_PX` / `TASK_INDENT_MAX_PX` in
  // `src-tauri/src/llm/markdown.rs`, which jodd-mcp imports rather than
  // re-declaring. Changing either number here means changing them there.
  function propagateChecklist(toggled: HTMLInputElement) {
    if (!editorEl) return;
    // Document order, any nesting depth; level from the task line's indent.
    const rows = (Array.from(editorEl.querySelectorAll('input[type=checkbox]')) as HTMLInputElement[]).map((cb) => {
      const line = cb.closest('div, li') as HTMLElement | null;
      const m = line ? parseInt(line.style.marginLeft || '0', 10) || 0 : 0;
      return { cb, level: Math.round(m / 28) };
    });
    const subtree = (i: number) => {
      const out: typeof rows = [];
      for (let j = i + 1; j < rows.length; j++) {
        if (rows[j].level > rows[i].level) out.push(rows[j]);
        else break;
      }
      return out;
    };
    const idx = rows.findIndex((r) => r.cb === toggled);
    if (idx >= 0) {
      const sub = subtree(idx);
      for (const r of sub) setCheckboxState(r.cb, toggled.checked); // parent → children
    }
    for (let i = rows.length - 1; i >= 0; i--) {
      const sub = subtree(i);
      if (sub.length) setCheckboxState(rows[i].cb, sub.every((r) => r.cb.checked)); // children → parent
    }
  }

  // The NEAREST block-level ancestor of `el` — the caret's line (a <div>,
  // checklist row, <li>, …). Nearest (not top-level) so a body wrapped in an
  // outer <div> indents the actual line, never the whole wrapper.
  function topBlock(el: HTMLElement | null): HTMLElement | null {
    let cur: HTMLElement | null = el;
    while (cur && cur !== editorEl) {
      if (['DIV', 'LI', 'P', 'H1', 'H2', 'H3', 'H4', 'BLOCKQUOTE'].includes(cur.tagName)) return cur;
      cur = cur.parentElement;
    }
    return null;
  }

  // Outline indent. List items nest via execCommand (→ nested <ul>/<ol>, which
  // round-trips to Apple). Other lines (plain paragraphs, checklists) indent via
  // margin-left in fixed steps.
  function indentSelection(outdent: boolean) {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0 || !editorEl) return;
    const node = sel.getRangeAt(0).startContainer;
    const el = (node.nodeType === 3 ? node.parentElement : node) as HTMLElement | null;
    const li = el?.closest('li');
    if (li && editorEl.contains(li)) {
      document.execCommand(outdent ? 'outdent' : 'indent');
    } else {
      const block = topBlock(el);
      // Only indent a SINGLE line. If the block contains other block-level
      // children it's a container holding many lines — indenting it would push
      // the whole note in (the bug just reported), so skip it.
      if (block && !block.querySelector('div, ul, ol, li, p, blockquote, table')) {
        const cur = parseInt(block.style.marginLeft || '0', 10) || 0;
        const next = Math.max(0, Math.min(168, cur + (outdent ? -28 : 28)));
        block.style.marginLeft = next === 0 ? '' : `${next}px`;
      } else {
        return; // nothing safe to indent → don't fire a save
      }
    }
    onInput();
  }

  // Local "connections graph" for the open note — its folder (child_of), tags
  // (tagged), outgoing + backlinks (mentions) laid out radially. Reads the data
  // the editor already has; the same relationships live in the fact-schema
  // `edges` table.
  let showGraph = false;
  let graphModal: { closeInto(pane: Pane): boolean } | undefined;
  // A node that navigates closes the graph INTO its destination pane, so
  // Android Back from there lands beneath the graph rather than reopening it.
  function closeGraphInto(pane: Pane) {
    if (!graphModal?.closeInto(pane)) showGraph = false;
  }
  // A folder or tag node lands on the list. Its title depends on the store
  // writes that follow the close (a tag node TOGGLES into the selection), so
  // it is named once they are done: a rename of the list the close landed on
  // — or, if the close was refused, the same push App's watchers would make.
  function titleGraphList() {
    navigateToPane('list', currentListTitle());
  }
  function gtrunc(s: string): string {
    return s.length > 16 ? s.slice(0, 15) + '…' : s;
  }
  $: graphNodes = (() => {
    if (!$selectedNote) return [] as any[];
    const items: any[] = [];
    if ($selectedNote.label) items.push({ kind: 'folder', label: $selectedNote.label });
    for (const t of bodyTags) items.push({ kind: 'tag', label: '#' + t });
    for (const n of connections.outgoing) items.push({ kind: 'link', label: n.title || 'Untitled', note: n });
    for (const n of connections.backlinks) items.push({ kind: 'backlink', label: n.title || 'Untitled', note: n });
    const N = items.length;
    const cx = 300, cy = 200, r = N <= 6 ? 125 : 160;
    return items.map((it, i) => {
      const a = (i / Math.max(1, N)) * 2 * Math.PI - Math.PI / 2;
      return { ...it, x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) };
    });
  })();
  // Every node in the graph is actionable, not just the note ones. Folder and
  // tag nodes make the same VIEW writes, in the same order, as a sidebar pick
  // (controllers/viewSelection.ts selectFolder / toggleTag), so the list they
  // land on is the one a sidebar click shows. They deliberately skip that
  // pick's note step (afterPick: open the view's first note on desktop and
  // tablet, clear it on the phone). The graph is drawn for the open note, and
  // both destinations contain it — a folder node is that note's own folder, a
  // tag node one of its own tags — so the note stays open (and selected in
  // the list) instead of being replaced by whichever note sorts first.
  function openGraphNode(gn: any) {
    if (gn.note) {
      closeGraphInto('note');
      openConnection(gn.note);
      return;
    }
    // The note's own account, not $currentAccount — the graph can be open on a
    // note belonging to a different account than the one the sidebar shows.
    const acct = $selectedNote?.account_id || $currentAccount;
    if (!acct) return;

    if (gn.kind === 'folder') {
      closeGraphInto('list');
      if ($currentAccount !== acct) currentAccount.set(acct);
      // Folder, tag and Smart Folder views are mutually exclusive, and folder
      // navigation also exits search — a "results across folders" view is
      // incoherent with "show me this folder". Same reasoning as the sidebar.
      if ($searchQuery) searchQuery.set('');
      navigateToFolder(gn.label);
      titleGraphList();
    } else if (gn.kind === 'tag') {
      closeGraphInto('list');
      if ($currentAccount !== acct) {
        currentAccount.set(acct);
        clearSelectedTags();
      }
      selectedSmartFolder.set(null);
      // Graph tag labels carry the display '#'; the store holds bare tags.
      toggleSelectedTag(gn.label.replace(/^#/, ''));
      titleGraphList();
    }
  }

  let editorEl: HTMLDivElement;
  // Bumped on every full body render so `{#key}` mounts a FRESH editor
  // element instead of reusing the old one via `innerHTML =`. WebKit's undo
  // history is a page-wide list of edit commands holding node references,
  // and `innerHTML =` neither clears it nor detaches the editor root — so a
  // stale step that re-inserts a node into the root lands in the new body.
  // Measured 2026-09-27 in scripts/webkit-editor-harness (render-undo.txt):
  // delete a line in note A, open note B, add a line, Cmd+Z twice → A's
  // deleted line appeared inside B. Against a fresh element the stale steps
  // act on the detached old root and change nothing on screen.
  let editorRenderKey = 0;
  let saveTimer: ReturnType<typeof setTimeout>;
  // True only after a REAL user edit (set in onInput, which fires on genuine
  // input events — NOT on programmatic innerHTML or the browser's re-serialize/
  // normalize of the loaded body). Gates all saves so merely viewing a note and
  // switching away can't spuriously insert-new + trash-old (which was sending
  // untouched notes' old revisions to Gmail Trash). Reset when a note loads.
  let userEdited = false;
  let editGeneration = 0;

  // The open note's permanently refused push (gotcha #14), from the row OR
  // the persisted state of its current version — the worker's refusal
  // reaches the frontend through the latter first. One rule (syncBlock) for
  // the toolbar, the metadata line and the persistence label: a refused push
  // must never read "Saved".
  $: syncBlockNow = $selectedNote ? syncBlock($selectedNote, $persistenceByNote[persistenceKey]) : null;

  // The open note's save state: one value that both the toolbar's {#if} chain
  // below branches on and the phone note PaneHeader shows in short form
  // (spec 2026-10-20 §6.1), so the two cannot disagree.
  $: saveState = $selectedNote ? shortSaveState({
    saving: $savingNoteKeys.has(persistenceKey),
    unsaved: userEdited || !!$pendingLocalEdits[persistenceKey],
    draft: !$selectedNote.uuid || $selectedNote.uuid.startsWith('tmp:'),
    blocked: !!syncBlockNow,
    savedLabel: saveLabel,
  }) : null;
  // Owned, not set: during a rotation's layout swap a second editor is alive,
  // and the one destroyed last must not wipe the live one's state.
  const saveStatePub = claimEditorSaveState();
  $: saveStatePub.set(saveState);
  onDestroy(() => saveStatePub.release());

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

  // What the BACKEND is confirmed to hold — the only honest answer to "is
  // this incoming body one we put there?", and deliberately NOT the same
  // thing as `lastPushedBody` above.
  //
  // `lastPushedBody` moves when `save_note` returns, and `save_note` is
  // local-first: it commits to SQLite and returns while the sync worker
  // pushes on a later tick. So between a local save and its push,
  // `lastPushedBody` names a body the backend has never seen. Anchoring
  // `isEcho` there meant a refresh landing in that gap (App.svelte's 10s
  // focus/folder settle reads the backend) delivered the PREVIOUS body —
  // matching neither the rendered body nor the "pushed" one — and the editor
  // announced "Edited on another device" for this device's own sync lag.
  //
  // Kept as a second variable rather than by fixing `lastPushedBody`'s
  // timing, because its other two readers genuinely want "last committed
  // LOCALLY": `hasPendingEdit` (or the editor would look dirty until a push
  // landed, blocking every re-render) and `autoSave`'s dedupe guard (or
  // type-then-undo would re-push identical content, and on Gmail every push
  // is an insert-new + trash-old).
  let lastRemoteBody: string | undefined;

  // The worker's receipt for a push it completed. Subscribed rather than
  // wired as a `$:` reaction so it cannot join the render block's dependency
  // graph: that block already re-runs on every keystroke, and giving it one
  // more input whose write it also reads is how the banner became a latch
  // with no release the first time (see `nextExternalChangeBanner`).
  onMount(() =>
    lastPushConfirmation.subscribe((confirmation) => {
      lastRemoteBody = pushedBodyAfterConfirmation({
        confirmation,
        editorUuid,
        editorAccountId,
        currentPushedBody: lastRemoteBody,
      });
    }),
  );

  // This note changed somewhere else while there was unsaved work here, so the
  // fresh content was deliberately NOT rendered (see shouldRenderExternalBody)
  // and the user needs telling. Latched: see nextExternalChangeBanner for why
  // the raise condition is an edge that cannot simply be recomputed.
  let externalChangePending = false;
  // Published for the phone note PaneHeader: on phone the toolbar row that
  // shows the banner does not render (spec 2026-10-20 §5.6), and the banner
  // must stay visible beside the save state (§6.1).
  const externalChangePub = claimEditorExternalChange();
  $: externalChangePub.set(externalChangePending ? { reload: reloadFromRemote } : null);
  onDestroy(() => externalChangePub.release());
  // Every body that raised (or re-raised) the banner since it last went down.
  // One can be this editor's own save, read from SQLite by a refresh after the
  // row was written and before `save_note`'s reply set `lastPushedBody` — the
  // same window `heldBackNotes` is released from in `doSaveNote`. The banner
  // latches, so the reply must take it down too, and only when EVERY body that
  // raised it is accounted for: a foreign edit arriving in the same window
  // keeps it up. Exact bytes, because the row holds exactly what was sent.
  const externalChangeBodies = new Set<string>();
  function logBanner(event: string, note: Note, flags: Record<string, boolean>) {
    const line = bannerDiagnostic({
      event, uuid: note.uuid, accountId: note.account_id, localVersion: note.local_version,
      incoming: note.body_html, lastPushedBody, lastRemoteBody, lastRenderedBody, flags,
    });
    console.log(`[jodd] ${line}`);
    void Promise.resolve(invoke('editor_diagnostic', { line })).catch(() => {});
  }
  // Per note (noteKey), the `local_version` this editor's text for it derives
  // from — what a save sends as `expectedLocalVersion`. NOT the store row's:
  // a held-back remote change replaces that row (and, after a keep-both
  // reconcile, its version) while the text on screen is still based on the
  // old one, and saving with the refreshed version made SQLite's
  // compare-and-swap accept a write over the other device's edit.
  //
  // Moves only in a pass that holds nothing back (a render, an echo of our
  // own save, a version bump that changed nothing shown — a move), and on this
  // editor's own confirmed save. Keyed per note rather than one variable
  // because `flushPendingEdit` saves the PREVIOUS note after the selection has
  // already moved on, and queued saves run later still.
  const saveBaseVersions = new Map<string, number | undefined>();
  // Notes whose remote change this editor is holding back right now, with the
  // body that was held back. A save for one of these must not land on the
  // note — see `isVersionRefusal`. Per note for the same reason: the flush on a
  // note switch runs after the banner for that note has already been cleared.
  //
  // Narrower than the banner on purpose. The banner compares bytes and has a
  // known false positive (iCloud returns our own push re-encoded); it costs a
  // dismissible line. A mark here costs a conflict copy, so a change is marked
  // only when it differs in SUBSTANCE from what this device last saved or saw
  // confirmed (`sameInSubstance`).
  const heldBackNotes = new Map<string, string>();
  // Bumped by reloadFromRemote() purely to re-run the render decision below.
  // Neither condition that button clears is reliably observable as a change:
  // DOM focus is not reactive at all, and a note that was focused but not yet
  // typed in already has lastPushedBody === lastRenderedBody, so writing them
  // assigns the same value and Svelte's safe_not_equal skips the invalidate.
  // Without this the button silently did nothing in exactly that case.
  let externalReloadNonce = 0;

  // The Note object the editor is currently bound to. Distinct from
  // $selectedNote because $selectedNote may have already changed to a NEW
  // note inside the reactive block — we need the OLD note's identity to
  // flush a pending save before the swap.
  let editorNote: Note | null = null;
  let emptyEditorEl: HTMLDivElement | undefined;
  async function closeContextNote() {
    selectedNote.set(null); // existing selection path flushes through the save chain
    await tick();
    if (!get(selectedNote)) emptyEditorEl?.focus();
  }

  // Re-render decisions:
  //   - UUID changed → user selected a different note → always re-render,
  //     but flush any pending edit for the OLD note first (or it's lost forever)
  //   - Same UUID but body changed externally (poll picked up Apple Notes' edit)
  //     → re-render, BUT only when safe (not saving, not typing)
  //   - Same UUID, body matches what we last wrote out → no re-render
  $: if ($selectedNote) {
    void externalReloadNonce; // dependency only — see its declaration
    const uuidChanged = !sameNote($selectedNote, editorNote);
    if (!uuidChanged && editorNote && editorUuid !== $selectedNote.uuid) {
      editorUuid = $selectedNote.uuid;
      editorNote = { ...editorNote, uuid: $selectedNote.uuid };
    }
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
    // Broader than activeIsInEditor: catches unsaved work even when DOM
    // focus is elsewhere in the note-editing surface (e.g. the title
    // <input>, a sibling of editorEl) or transiently lost (window
    // blur/refocus). Without this, editing the title right after editing
    // the body — focus now outside editorEl, autosave still debouncing —
    // let a background poll/settle/sweep overwrite editorEl.innerHTML with
    // the stale pre-edit body, silently discarding the edit and resetting
    // WKWebView's native undo stack.
    const hasPendingEdit =
      lastRenderedBody !== lastPushedBody ||
      (typeof title === 'string' && title !== lastPushedTitle);
    const shouldRender = shouldRenderExternalBody({
      uuidChanged,
      bodyChanged,
      isSaving: $isSaving,
      activeIsInEditor,
      hasPendingEdit,
    });

    // P1: what arrived is another device's APPEND to text this editor already
    // has (`appendedSuffix`) — Share to Jodd's dated blocks landing while the
    // note is open, either merged into the dirty row by reconcile or pulled
    // into a clean one. Holding it back would end in a conflict copy; instead
    // the appended blocks join the end of the editor, in this same pass
    // (gotcha #28): no banner, no held-back mark, no re-render of the user's
    // text (the caret and undo stay). The next save is based on the row that
    // carried the append, so SQLite's compare-and-swap accepts it. The base is
    // what this editor last SAVED — never an older body (see `appendedSuffix`).
    // Never while this note's own save is in flight: a refresh can read that
    // save from SQLite before its reply arrives, and it too is "the last saved
    // body plus an ending" — the user's own text, which they may since have
    // deleted. That echo has its own handling (the banner's save-reply release).
    // Nor on a body already merged: the merge's own refresh, or any re-read of
    // the same row before the save lands, hands it back (equal version,
    // different body) — appending again would double it.
    const appended =
      !uuidChanged && bodyChanged && !shouldRender && !externalChangePending && editorEl
      && !get(savingNoteKeys).has(noteKey($selectedNote))
      && $selectedNote.body_html !== lastRemoteBody
        ? appendedSuffix(lastPushedBody ?? lastRemoteBody, $selectedNote.body_html)
        : null;
    if (appended !== null && editorEl) {
      const incoming = $selectedNote.body_html;
      saveBaseVersions.set(noteKey($selectedNote), $selectedNote.local_version);
      lastRemoteBody = incoming;
      editorEl.insertAdjacentHTML('beforeend', appended);
      scheduleLinkScan(); // the appended blocks may carry links
      if (hasPendingEdit) {
        onInput(); // the user's text plus the append is now the edit to save
      } else {
        // Nothing of the user's is pending: the row already IS the editor.
        lastRenderedBody = incoming;
        lastPushedBody = incoming;
      }
    }
    const bodyChangedHere = bodyChanged && appended === null;

    // The other half of the same decision. `shouldRender === false` here means
    // content arrived and we refused to apply it because the user is mid-edit
    // — correct, and until now silent: they went on typing against a body the
    // server had moved past, and learned about it when the reconciler minted a
    // conflict copy behind them.
    const bannerWasShown = externalChangePending;
    // Ours twice over: the body the backend confirmed, OR the body this
    // editor last committed to SQLite. A refresh answers from the cache row,
    // which holds the local save until the worker pushes it — comparing
    // against the remote alone blamed another device for that gap
    // (noteEditorLocalSaveEcho.test.ts). Neither comparison alone suffices.
    const isEcho =
      $selectedNote.body_html === lastRemoteBody ||
      $selectedNote.body_html === lastPushedBody;
    externalChangePending = nextExternalChangeBanner({
      shown: externalChangePending,
      uuidChanged,
      willRender: shouldRender,
      bodyChanged: bodyChangedHere,
      isEcho,
    });
    // Same turn as the banner (gotcha #28): the save base and the held-back
    // mark are both functions of it, and a pass landing between them would
    // save with one and not the other.
    // Marked on the pass a change ARRIVES, not on every pass while the banner
    // is latched: those passes are keystrokes, and by then the store holds the
    // editor's own text.
    const raisedBy = externalChangePending && !uuidChanged && bodyChangedHere && !isEcho
      && !externalChangeBodies.has($selectedNote.body_html);
    if (!externalChangePending) externalChangeBodies.clear();
    else if (raisedBy) externalChangeBodies.add($selectedNote.body_html);
    // Names what raised or cleared it, into jodd.log. Three conditions can hold
    // a change back, they resolve in completely different ways, and the banner
    // looks identical for all of them — and it went up falsely after five fixes
    // with its reason visible only in a console a release build does not have.
    if (raisedBy || (bannerWasShown && !externalChangePending)) {
      logBanner(
        !raisedBy ? (uuidChanged ? 'CLEARED-by-switch' : 'CLEARED-by-render') : bannerWasShown ? 'RE-RAISED' : 'RAISED',
        $selectedNote,
        { isSaving: $isSaving, activeIsInEditor, hasPendingEdit, saveInFlight: get(savingNoteKeys).has(noteKey($selectedNote)) },
      );
    }
    const selectedKey = noteKey($selectedNote);
    if (!externalChangePending) {
      heldBackNotes.delete(selectedKey);
      saveBaseVersions.set(selectedKey, $selectedNote.local_version);
    } else if (
      !uuidChanged && bodyChangedHere && !isEcho &&
      !sameInSubstance($selectedNote.body_html, lastPushedBody) &&
      !sameInSubstance($selectedNote.body_html, lastRemoteBody)
    ) {
      heldBackNotes.set(selectedKey, $selectedNote.body_html);
    }

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
            const dropNote = editorNote;
            notes.update((ns) => ns.filter((n) => !sameNote(n, dropNote)));
          }
        }
      }
      clearTimeout(saveTimer);
      if (uuidChanged) {
        // Invalidate before the deferred DOM refresh: no previous account's evidence
        // may appear beneath the new title while its local read is pending.
        citeSeq++; connSeq++;
        citations = []; connections = { outgoing: [], backlinks: [] };
        // Same turn for the link router: no previous note's chip, preview or
        // late resolve survives into this one.
        linkEpoch++; linkOpenSeq++;
        linkPreviews = new Map(); linkPreviewsInFlight = new Set();
        closeLinkChip();
        // The previous note's tags must not reach the metadata line's #N (or
        // the tag row) for the pass before the tick below re-reads them.
        bodyTags = [];
        citationsLoading = false; citationsFailed = false; citationsSettled = false;
      }
      editorNote = $selectedNote;
      editorUuid = $selectedNote.uuid;
      title = typeof $selectedNote.title === 'string' ? $selectedNote.title : '';
      // Defer innerHTML until after Svelte's DOM update (bind:this timing).
      const targetUuid = $selectedNote.uuid;
      const targetBody = $selectedNote.body_html;
      const targetAccount = $selectedNote.account_id ?? $currentAccount;
      const targetId = $selectedNote.id;
      // `.editor-body` is its own scroll container, and the fresh element below
      // starts at 0. A same-note render is a refresh (on Gmail, every push reads
      // back as new bytes), so it must land where the user was reading — or the
      // note jumps to the top a few seconds after they type at the bottom.
      const keepScrollTop = !uuidChanged && editorEl ? editorEl.scrollTop : 0;

      // Baselines move WITH the identity, synchronously — never on the other
      // side of the tick. Only `editorEl.innerHTML` needs the DOM (bind:this
      // timing); these three are pure functions of `targetBody`, which is in
      // hand already.
      //
      // Deferring them split one state change across two turns: `editorUuid`
      // updated now, the baselines a microtask later. Any reactive pass in the
      // gap saw `uuidChanged` false (so the banner was not cleared),
      // `bodyChanged` true and `isEcho` false against the PREVIOUS note's body
      // — and "Edited on another device" latched with no other device in
      // existence. Measured live 2026-09-05: a 266-byte note compared against
      // a 383-byte body belonging to the note viewed before it.
      //
      // The gap needs no external trigger, which is why it fired on every
      // open: `title = …` immediately below is itself a reactive write, so the
      // block re-enters on its own before a deferred assignment could land.
      lastRenderedBody = targetBody;
      // Initial render = the server's confirmed state for this note.
      // Anchor the "did anything change since push?" comparison here.
      const pendingDraft = get(pendingLocalEdits)[noteKey($selectedNote)];
      lastPushedBody = pendingDraft ? undefined : targetBody;
      lastPushedTitle = pendingDraft ? undefined : title;
      // The body being rendered came from a read, so it is also the
      // backend's state as of now — the anchor for a note whose push
      // predates this editor ever opening it.
      lastRemoteBody = targetBody;
      // Fresh note loaded — no pending user edit yet. Critical: prevents a
      // view-then-switch from being treated as an edit (see flushPendingEdit).
      userEdited = !!pendingDraft;
      // A fresh element for the new body (see editorRenderKey), and nothing
      // that points into the old one survives it.
      editorRenderKey++;
      lastTrigger = null;
      closeHashtagPicker();
      bodyRenderedKey = null;

      tick().then(() => {
        if (editorEl && sameNote(editorNote, { account_id: targetAccount, uuid: targetUuid })) {
          editorEl.innerHTML = extractBody(targetBody);
          if (keepScrollTop) editorEl.scrollTop = keepScrollTop;
          scheduleLinkScan(); // the new body's links
          refreshBodyTags(); // chips reflect the freshly-loaded note's #hashtags
          bodyRenderedKey = noteKey({ account_id: targetAccount, uuid: targetUuid });
          applyHandedOffTagEdits(); // a rotation's queued tag edits, if any
          refreshConnections(); // backlinks / outgoing links for this note
          refreshCitations(); // URLs this note cites
          // Render inline images + run the stale-body safety net. Called
          // unconditionally (not gated on cid in body) so a stale body with no
          // refs but stored attachments is detected and healed.
          if (targetAccount) {
            hydrateAttachments(targetUuid, targetAccount, targetId);
          }
        }
      });
    }
  } else {
    // No note selected → flush any pending edit before clearing state
    if (editorNote && editorEl) {
      flushPendingEdit(editorNote, title, wrapBody(editorEl.innerHTML));
    }
    clearTimeout(saveTimer);
    citeSeq++; connSeq++;
    citations = []; connections = { outgoing: [], backlinks: [] };
    citationsLoading = false; citationsFailed = false; citationsSettled = false;
    linkEpoch++; linkOpenSeq++;
    linkPreviews = new Map(); linkPreviewsInFlight = new Set();
    closeLinkChip();
    editorNote = null;
    editorUuid = undefined;
    lastRenderedBody = undefined;
  }

  // Fire-and-forget save with an explicit note identity (not $selectedNote).
  // Called when the user switches notes before the debounced autosave fires —
  // the captured `note` parameter carries the previous note's id/uuid/label
  // so the save targets the right Gmail message, not whichever note is
  // currently selected after the swap.
  // Destroyed with an autosave pending — typing within 1.5 s of a rotation
  // (App mounts a separate editor per layout) or of the phone's Back from the
  // note pane. The timer used to outlive the editor and fire autoSave on it
  // later, saving this editor's body and title into whatever `$selectedNote`
  // was by then: another note, if the user had opened one (found 2026-10-21).
  // Save the pending edit now, against this editor's own note, as a note
  // switch does; flushPendingEdit skips a note nobody edited.
  onDestroy(() => {
    clearTimeout(saveTimer);
    if (editorNote && editorEl) flushPendingEdit(editorNote, title, wrapBody(editorEl.innerHTML));
  });

  function flushPendingEdit(note: Note, noteTitle: string, bodyHtml: string) {
    if (deletingNotes.has(noteKey(note))) return;
    // No genuine user edit on this note → never save. Without this, the
    // browser's innerHTML re-serialization (attribute order, whitespace,
    // entities) makes a merely-VIEWED note's body differ from lastPushedBody,
    // which the string compare below reads as a change → spurious insert-new +
    // trash-old → the untouched note's old revision lands in Gmail Trash.
    if (!userEdited) return;
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

  // Includes content an older build stranded after `</html>` (gotcha #37):
  // otherwise the editor hides it and its next save drops it.
  function extractBody(html: string): string {
    return bodyContent(html);
  }

  // Convert rendered inline images back to Apple's <object cid:…> form before
  // storing/saving, so the persisted body stays Apple-canonical and round-trips
  // (and matches lastPushedBody, avoiding spurious re-saves).
  function imgsToObjects(inner: string): string {
    return inner.replace(
      /<img[^>]*\bdata-jodd-cid="([^"]+)"[^>]*>/gi,
      (_m, cid) =>
        `<object type="application/x-apple-msg-attachment" data="cid:${cid}"></object>`
    );
  }

  function wrapBody(inner: string): string {
    return `<html><head></head><body style="overflow-wrap: break-word; -webkit-nbsp-mode: space; line-break: after-white-space;">${imgsToObjects(inner)}</body></html>`;
  }

  // Notes we've already tried to heal this session — prevents a refetch loop on
  // a note whose remote body is ALSO empty (already-damaged: edited+pushed while
  // its cache body was stale).
  const healedAttachmentNotes = new Set<string>();

  // After a note renders: render image attachments inline (swap <object cid:…>
  // for <img>), AND act as the data-loss safety net — if the note has stored
  // attachments but the body references NONE, the body is stale/over-stripped
  // (parsed before the title+image fix); editing it would push an
  // attachment-less message and lose them. Force one re-parse from Gmail to heal
  // before any edit. Guards against the selected note changing mid-fetch.
  async function hydrateAttachments(uuid: string, accountId: string, noteId: string) {
    type Att = { content_id: string; mime_type: string; data_uri: string };
    let atts: Att[] = [];
    try {
      atts = await invoke<Att[]>('get_note_attachments', { accountId, uuid });
    } catch (e) {
      return;
    }
    if (!editorEl || !sameNote(editorNote, { account_id: accountId, uuid }) || !atts.length) return;

    const objects = editorEl.querySelectorAll('object[data^="cid:"]');
    if (objects.length === 0) {
      // Stale body — heal once via refetch (re-parses with the fixed strip).
      if (noteId && !healedAttachmentNotes.has(noteKey({ account_id: accountId, uuid }))) {
        healedAttachmentNotes.add(noteKey({ account_id: accountId, uuid }));
        try {
          const fresh = await invoke<Note>('refetch_note', { accountId, id: noteId });
          notes.update((ns) => {
            const i = ns.findIndex((n) => sameNote(n, fresh));
            if (i >= 0) ns[i] = fresh;
            return ns;
          });
          // Re-renders + re-hydrates via the reactive load block (now with refs).
          if (sameNote(get(selectedNote), fresh)) selectedNote.set(fresh);
        } catch (e) {
          // Offline, or remote also empty (already-damaged) — leave as-is.
        }
      }
      return;
    }

    // Render images inline; non-image attachments keep their <object>.
    const byCid = new Map(atts.filter((a) => a.data_uri).map((a) => [a.content_id, a]));
    objects.forEach((obj) => {
      const cid = (obj.getAttribute('data') || '').slice(4); // strip "cid:"
      const a = byCid.get(cid);
      if (!a) return;
      const img = document.createElement('img');
      img.src = a.data_uri;
      img.setAttribute('data-jodd-cid', cid);
      img.style.maxWidth = '100%';
      img.style.height = 'auto';
      obj.replaceWith(img);
    });
  }

  function onInput(e?: Event) {
    // The user's own edit invalidates the offsets Replace remembered; Replace's
    // own edits (findReplacing) are what it is remembering.
    if (!findReplacing) forgetReplaced();
    closeLinkChip(); // typing closes the link chip
    scheduleLinkScan(); // links follow the text, one scan per frame
    ++editGeneration;
    userEdited = true;
    // Defer markdown-trigger detection by a frame: under accessibility-API
    // text input (e.g. computer-use 'type'), the input event fires BEFORE
    // WebKit finishes updating the DOM, so reading textContent/cursor offset
    // immediately would see stale state. requestAnimationFrame guarantees the
    // browser has painted before we read.
    const inputEvt = e as InputEvent | undefined;
    requestAnimationFrame(() => {
      if (!editorEl) return;
      const applied = applyTriggerAtCaret(editorEl, inputEvt);
      if (applied) lastTrigger = applied;
    });
    if (editorEl) ensureCheckboxesNotEditable(editorEl);
    refreshBodyTags(); // keep chip row in sync as #hashtags are typed/edited
    detectLinkPicker(); // open the [[ note-link autocomplete when relevant
    detectHashtagPicker(); // and the # tag autocomplete (after: [[ wins)
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
      pendingLocalEdits.update(m => ({ ...m, [noteKey(updated)]: { title: liveTitle, body_html: liveBody } }));
      lastRenderedBody = liveBody;
      notes.update((ns) => {
        const idx = ns.findIndex((n) => sameNote(n, updated));
        if (idx >= 0) ns[idx] = updated;
        return ns;
      });
      selectedNote.set(updated);
    }
  }

  // True when the title starts with an orphaned combining mark (e.g. a lone
  // Thai sara-uee, U+0E37) — a character that renders BLANK in Jodd on Windows
  // (WebView2) while iCloud shows it as a dotted circle plus the mark. The user
  // cannot see it to delete it, so the banner below surfaces it (rendered the
  // way iOS does) with a one-click remove. See src/lib/textDisplay.ts.
  $: titleHasOrphanMark = startsWithOrphanCombiningMark(typeof title === 'string' ? title : '');

  // Strips the leading orphaned combining marks — an EXPLICIT user action
  // (never silent), then persists through the ordinary input/save path.
  function removeLeadingOrphanMarks() {
    if (typeof title !== 'string') return;
    title = title.replace(/^\p{M}+/u, '');
    onInput();
  }

  // A fresh draft's title is the stand-in "New Note", not the user's text —
  // but it stays the stored default: the untouched-draft guards (flush, the
  // ghost-row drop, isEmptyDraft) and every backend's title round trip
  // (gotchas #11 / #17 / #27) count on a non-empty title. So while the note
  // is an untouched draft, focusing the title selects it and typing replaces
  // it (live pass 2026-10-21: typing appended — "New Live pass…").
  function titleIsDraftDefault(): boolean {
    return !!$selectedNote?.uuid?.startsWith('tmp:') && title === 'New Note';
  }
  // A touch tap focuses the field and only then places the caret where the
  // finger was (its click), which would undo the focus's selection: the first
  // click after such a focus selects again.
  let reselectTitleOnClick = false;
  function onTitleFocus() {
    if (!titleIsDraftDefault() || !titleEl) return;
    titleEl.select();
    reselectTitleOnClick = true;
  }
  function onTitleClick() {
    if (!reselectTitleOnClick) return;
    reselectTitleOnClick = false;
    if (titleIsDraftDefault()) titleEl?.select();
  }

  function onTitleKeydown(e: KeyboardEvent) {
    reselectTitleOnClick = false;
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

  // The title is a textarea (so it wraps) holding one line. A soft
  // keyboard's Enter may arrive as a line-break input rather than a keydown.
  function onTitleBeforeInput(e: InputEvent) {
    if (e.inputType === 'insertLineBreak' || e.inputType === 'insertParagraph') {
      e.preventDefault();
      reselectTitleOnClick = false;
      editorEl?.focus();
    }
  }
  function onTitleInput(e: Event) {
    const el = e.currentTarget as HTMLTextAreaElement;
    if (/[\r\n]/.test(el.value)) {
      el.value = el.value.replace(/\r\n|\r|\n/g, ' ');
      title = el.value;
    }
    fitTitle();
    onInput();
  }

  // `field-sizing: content` grows the title with its text. A webview without
  // it (macOS WebKit before Safari 26) would keep a rows=1 textarea one line
  // tall and hide the rest, so there the height follows scrollHeight: on
  // input, when the note's title is set, and when the width changes.
  const titleSizesItself =
    typeof CSS !== 'undefined' && typeof CSS.supports === 'function' && CSS.supports('field-sizing', 'content');
  function fitTitle() {
    const el = titleEl;
    if (titleSizesItself || !el) return;
    const cs = getComputedStyle(el);
    const pad = (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0);
    el.style.height = 'auto';
    el.style.height = `${Math.max(el.scrollHeight - pad, 0)}px`;
  }
  $: refitTitle(titleEl, title);
  function refitTitle(..._deps: unknown[]) {
    if (!titleSizesItself) void tick().then(fitTitle);
  }
  let titleWidth = 0;
  let titleResize: ResizeObserver | null = null;
  $: if (titleEl && !titleSizesItself && !titleResize && typeof ResizeObserver !== 'undefined') {
    titleResize = new ResizeObserver(() => {
      const w = titleEl?.clientWidth ?? 0;
      if (w === titleWidth) return;
      titleWidth = w;
      fitTitle();
    });
    titleResize.observe(titleEl);
  }
  onDestroy(() => titleResize?.disconnect());

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
    if (bodyHtml === lastPushedBody && titleStr === lastPushedTitle) {
      acknowledgeLocalEdit({ ...$selectedNote, title: titleStr, body_html: bodyHtml });
      userEdited = false;
      return;
    }

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

    const target = $selectedNote;
    const generation = editGeneration;
    const ok = await saveNote(target, title, bodyHtml);
    // Only clear the pending-edit flag on a CONFIRMED save. A rejected save
    // (a real conflict, or a stale-version false conflict) must leave
    // userEdited true so a later flush/autosave retries instead of treating
    // a failed write as if it had landed — otherwise the edit is silently
    // lost the moment the user navigates away without typing again.
    if (ok && sameNote(get(selectedNote), target) && generation === editGeneration) {
      userEdited = false;
      refreshConnections(); // [[links]] just saved → refresh the graph panel
      refreshCitations(); // URLs just saved → refresh the sources panel
    }
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

  // Returns whether the save actually landed. Callers that need to know
  // whether it's safe to treat the edit as flushed (autoSave clearing
  // userEdited) must check this instead of assuming a resolved promise
  // means success — doSaveNote deliberately never rejects (it catches
  // internally so one failed save doesn't break the chain for the next
  // queued save), so "didn't throw" is NOT the same as "succeeded".
  function saveNote(note: Note, noteTitle: string, bodyHtml: string): Promise<boolean> {
    const epoch = deletionEpoch(note);
    const next = savePromiseChain
      .catch(() => {}) // previous failure doesn't block this one
      .then(() => deletionEpoch(note) === epoch ? doSaveNote(note, noteTitle, bodyHtml) : false);
    savePromiseChain = next.catch(() => {});
    return next;
  }

  async function doSaveNote(note: Note, noteTitle: string, bodyHtml: string): Promise<boolean> {
    // Defensive: if any of these are the wrong type, Tauri's serde rejects
    // the invoke with a cryptic "invalid type: map, expected a string" that
    // tells us nothing about which value was wrong. Catch it here with a
    // useful message so we can diagnose.
    if (typeof noteTitle !== 'string') {
      const sample = (() => { try { return JSON.stringify(noteTitle).slice(0, 200); } catch { return String(noteTitle); } })();
      error.set(`saveNote refused: noteTitle is ${typeof noteTitle}, expected string. Value: ${sample}`);
      console.error('saveNote: noteTitle bad type', noteTitle);
      return false;
    }
    if (typeof bodyHtml !== 'string') {
      const sample = (() => { try { return JSON.stringify(bodyHtml).slice(0, 200); } catch { return String(bodyHtml); } })();
      error.set(`saveNote refused: bodyHtml is ${typeof bodyHtml}, expected string. Value: ${sample}`);
      console.error('saveNote: bodyHtml bad type', bodyHtml);
      return false;
    }
    if (!note || typeof note !== 'object') {
      error.set(`saveNote refused: note is ${typeof note}, expected object`);
      console.error('saveNote: note bad type', note);
      return false;
    }

    // Re-read the latest note from the store. If a previous queued save
    // updated this note's Gmail id, we want to use the freshest id — not
    // the one captured when this save was queued. Without this re-read,
    // the queue serialization helps but the second save still tracks the
    // stale id that the first save just replaced.
    note = { ...note, uuid: canonicalNote(note).uuid };
    const cur = get(selectedNote);
    if (cur && sameNote(cur, note)) {
      note = {
        ...note,
        id: cur.id,
        x_mail_created_date: cur.x_mail_created_date ?? note.x_mail_created_date,
        // Repair local_version too, not just id — a queued second autosave
        // must CAS against the version the FIRST save just landed, not the
        // pre-first-save snapshot it was queued with. Without this, save #2
        // sends a stale expectedLocalVersion and gets a false conflict
        // against this app's own prior save, not a real external writer.
        local_version: cur.local_version ?? note.local_version,
      };
    } else {
      const fromArr = get(notes).find((n) => sameNote(n, note));
      if (fromArr) {
        note = {
          ...note,
          id: fromArr.id,
          x_mail_created_date: fromArr.x_mail_created_date ?? note.x_mail_created_date,
          local_version: fromArr.local_version ?? note.local_version,
        };
      }
    }
    // …but the version the TEXT derives from, when this editor knows it. The
    // store row can already be a remote change the editor held back, and its
    // version is the one thing the compare-and-swap below must not be handed.
    // The queued-second-autosave case above still holds: a confirmed save
    // advances this entry before the next queued save reads it.
    if (saveBaseVersions.has(noteKey(note))) {
      note = { ...note, local_version: saveBaseVersions.get(noteKey(note)) };
    }

    const savingEpoch = deletionEpoch(note);
    const savingKey = noteKey(note);
    savingNoteKeys.update(s => new Set([...s, savingKey]));
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
        return false;
      }

      // Never write held-back text over the note — see `isVersionRefusal`.
      if (existingUuid && heldBackNotes.has(savingKey)) {
        return await saveAsConflictCopy(note, noteTitle, bodyHtml, accountId, label);
      }
      let saved: { id: string; uuid: string; local_version: number };
      try {
        saved = await invoke<{ id: string; uuid: string; local_version: number }>('save_note', {
          accountId,
          title: noteTitle,
          bodyHtml,
          existingGmailId,
          existingUuid,
          existingXMailCreatedDate,
          label,
          // Only meaningful when editing an existing note — a brand-new note
          // (no existingUuid yet) has no prior version to guard against.
          expectedLocalVersion: existingUuid ? (note.local_version ?? null) : null,
        });
      } catch (e) {
        if (existingUuid && isVersionRefusal(e)) {
          return await saveAsConflictCopy(note, noteTitle, bodyHtml, accountId, label);
        }
        throw e;
      }

      if (deletionEpoch(note) !== savingEpoch) return true;
      rekeyNoteStores(accountId, note.uuid, saved.uuid);
      const savedKey = noteKey({ account_id: accountId, uuid: saved.uuid });
      saveBaseVersions.set(savedKey, saved.local_version);
      // A change held back while THIS save was in flight can be this save, read
      // from SQLite before its reply arrived. Now that it has, it is ours.
      if (sameInSubstance(heldBackNotes.get(savedKey), bodyHtml)) heldBackNotes.delete(savedKey);
      // The banner is the other half of that decision — see `externalChangeBodies`.
      const shown = get(selectedNote);
      if (externalChangePending && shown && sameNote(shown, note) && externalChangeBodies.delete(bodyHtml)) {
        const released = externalChangeBodies.size === 0;
        logBanner(released ? 'RELEASED-by-save-reply' : 'KEPT-after-save-reply', { ...shown, body_html: bodyHtml }, { othersPending: !released });
        if (released) externalChangePending = false;
      }
      const updatedNote: Note = {
        ...note,
        id: saved.id,
        uuid: saved.uuid,
        title: noteTitle,
        body_html: bodyHtml,
        date: new Date().toISOString(),
        label,
        x_mail_created_date: existingXMailCreatedDate,
        account_id: accountId,
        local_version: saved.local_version,
      };

      const live = get(selectedNote);
      acknowledgeLocalEdit(updatedNote);
      // A closed note can still have a newer queued draft. Advancing the saved
      // identity/version must not replace that draft with an older acknowledgement:
      // the following serialized write may fail and the user must be able to reopen
      // their latest text. The account-qualified draft map also follows UUID aliases.
      const pendingDraft = get(pendingLocalEdits)[noteKey(updatedNote)];
      const visibleNote = live && sameNote(live, updatedNote)
        ? { ...updatedNote, title: live.title, body_html: live.body_html }
        : pendingDraft ? { ...updatedNote, ...pendingDraft } : updatedNote;
      recordLocalSave(updatedNote);
      void refreshNotePersistence(updatedNote);
      notes.update(ns => {
        // Match on the uuid we SENT as well as the one we got back, because
        // those two differ whenever Rust resolved the save onto a different
        // row than the one we addressed. Two cases, one rule:
        //
        //  - First save of a new note: we sent a tmp: uuid, Rust minted a
        //    real one.
        //  - A note the worker REKEYED: the backend assigned the identity at
        //    create time (Exchange's internetMessageId), so the row moved to
        //    a new PRIMARY KEY while this editor kept the old uuid. Rust
        //    follows its forwarding address and echoes the note's CURRENT
        //    uuid — see `save_note_db` in lib.rs.
        //
        // Matching only on `saved.uuid` misses both, so the note falls to
        // the `else` below and is PREPENDED: the same note twice in the
        // list, one entry stuck at a uuid that no longer exists.
        const isThisNote = (n: Note) =>
          n.account_id === accountId && (n.uuid === saved.uuid || sameNote(n, note) || (tmpUuidToReplace !== null && n.uuid === tmpUuidToReplace));
        const idx = ns.findIndex(isThisNote);
        if (idx < 0) return [visibleNote, ...ns];
        // Collapse any OTHER entry for the same note before writing. After a
        // rekey the list can legitimately hold two: the stale one this
        // editor is still using, and the new-uuid one a background poll
        // already pulled in. Keeping the found position stops the note from
        // jumping to the top of the list on an ordinary autosave.
        const collapsed = ns.filter((n, i) => i === idx || !isThisNote(n));
        collapsed[collapsed.findIndex(isThisNote)] = visibleNote;
        return collapsed;
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
      if (sameNote(cur, note)) {
        // ALSO sync editorUuid/editorNote BEFORE selectedNote.set fires the
        // reactive block. Otherwise: when we save a brand-new note (whose
        // captured uuid was ''), the resulting selectedNote.set(updatedNote)
        // changes $selectedNote.uuid to a real value, the reactive block
        // sees uuidChanged=true and fires flushPendingEdit(blank_note, ...),
        // which invokes save_note AGAIN with empty existing_uuid → Rust
        // generates ANOTHER fresh UUID → a 2× duplicate of the new note.
        // Keeping editorUuid/editorNote in lockstep with selectedNote means
        // the reactive block sees "no change, no flush needed."
        lastRenderedBody = visibleNote.body_html;
        // Anchor the "did anything change since push?" check to what we just
        // confirmed on Gmail. Without this, the next autoSave would compare
        // against lastRenderedBody (which moves on every keystroke) and skip
        // a legitimate edit because "live editor == lastRenderedBody" by
        // construction.
        lastPushedBody = updatedNote.body_html;
        lastPushedTitle = updatedNote.title;
        editorUuid = updatedNote.uuid;
        editorNote = visibleNote;
        selectedNote.set(visibleNote);
        if (visibleNote.title === updatedNote.title && visibleNote.body_html === updatedNote.body_html) userEdited = false;
      }
      return true;
    } catch (e) {
      // A rejected save (a real conflict, or save_note's "changed elsewhere"
      // error) must be reported as a failure to the caller — NOT swallowed
      // as if the write had landed. See the callers (autoSave) for why:
      // treating this as success would clear the pending-edit flag and the
      // edit would never be retried, silently discarding it.
      error.set(String(e));
      return false;
    } finally {
      savingNoteKeys.update(s => { const next = new Set(s); next.delete(savingKey); return next; });
      isSaving.set(false);
    }
  }

  // The keep-both answer to a save that must not land on its note (see
  // `isVersionRefusal`): the text becomes a NEW note beside it, and the note
  // keeps the version that arrived from elsewhere. Same shape as the conflict
  // copy `reconcile_one_db` mints, created through the ordinary new-note save
  // so every backend pushes it the way it pushes any note.
  //
  // If the editor is still on the note, it follows its text into the copy in
  // lockstep — identity, baselines and selection in one turn, exactly like the
  // tmp:→real rekey in doSaveNote — so nothing re-renders: the caret and the
  // undo stack stay where they were, and the user keeps typing into the copy.
  async function saveAsConflictCopy(note: Note, noteTitle: string, bodyHtml: string, accountId: string, label: string): Promise<boolean> {
    const copyTitle = conflictCopyTitle(noteTitle, new Date());
    const saved = await invoke<{ id: string; uuid: string; local_version: number }>('save_note', {
      accountId,
      title: copyTitle,
      bodyHtml,
      existingGmailId: null,
      existingUuid: null,
      existingXMailCreatedDate: null,
      label,
      expectedLocalVersion: null,
    });
    const copy: Note = {
      ...note,
      id: saved.id,
      uuid: saved.uuid,
      title: copyTitle,
      body_html: bodyHtml,
      date: new Date().toISOString(),
      label,
      x_mail_created_date: null,
      account_id: accountId,
      pinned: false,
      local_version: saved.local_version,
      push_blocked_reason: null,
    };
    const primaryKey = noteKey(note), copyKey = noteKey(copy);
    heldBackNotes.delete(primaryKey);
    saveBaseVersions.set(copyKey, saved.local_version);
    // The draft under the note's key is this text; it belongs to the copy now,
    // and only if the user typed past what was just saved.
    pendingLocalEdits.update((m) => {
      const draft = m[primaryKey];
      const next = { ...m };
      delete next[primaryKey];
      if (draft && draft.body_html !== bodyHtml) next[copyKey] = { title: copyTitle, body_html: draft.body_html };
      return next;
    });
    recordLocalSave(copy);
    void refreshNotePersistence(copy);
    markRecentlySaved(accountId, copy.uuid);
    indexUpsertOnSave(accountId, null, saved.id, label);

    const live = get(selectedNote);
    const stillHere = !!live && sameNote(live, note);
    const visibleCopy = stillHere ? { ...copy, body_html: live!.body_html } : copy;
    notes.update((ns) => [visibleCopy, ...ns.filter((n) => !sameNote(n, copy))]);
    if (stillHere) {
      lastRenderedBody = visibleCopy.body_html;
      lastPushedBody = bodyHtml;
      lastPushedTitle = copyTitle;
      title = copyTitle;
      editorUuid = copy.uuid;
      editorNote = visibleCopy;
      externalChangePending = false;
      selectedNote.set(visibleCopy);
      userEdited = visibleCopy.body_html !== bodyHtml;
    }
    error.set(`"${noteTitle}" was changed elsewhere while you were editing. Your version was saved as "${copyTitle}".`);
    void showNoteAsStored(note, accountId, label);
    return true;
  }

  // Put the note's own row back in the list. While its change was held back,
  // every keystroke wrote the editor's text into the list under the note's
  // key (onInput's optimistic update), and on the SQLite-refused path the list
  // never saw the other version at all — so neither is what the row holds.
  // A local read, not a remote one: the row is the truth of the moment.
  async function showNoteAsStored(note: Note, accountId: string, label: string) {
    try {
      const rows = await invoke<Note[]>('list_cached_notes_in_folder', { accountId, path: label });
      const stored = rows.find((n) => sameNote({ ...n, account_id: n.account_id ?? accountId }, note));
      if (!stored) return;
      notes.update((ns) => ns.map((n) => (sameNote(n, note) ? { ...stored, account_id: accountId } : n)));
    } catch { /* the next refresh repaints it */ }
  }

  // Load the version that arrived from elsewhere, discarding the unsaved edit
  // that was keeping it out.
  //
  // Deliberately does NOT force a render past `shouldRenderExternalBody`. It
  // clears the two conditions that made that function answer "unsafe" — the
  // pending edit and the focus — and lets the existing reactive block reach
  // its own conclusion. Overriding the policy would put a second, weaker copy
  // of the mid-edit-clobber rule in the codebase, and that rule is the one
  // whose absence cost a user's typing and WKWebView's undo stack.
  //
  // Clearing those conditions is NOT enough to make the block re-run, which
  // is what `externalReloadNonce` is for — see its declaration.
  //
  // A save in flight ($isSaving) still refuses, and correctly: the block re-runs
  // when the save settles and renders then. The banner stays up until it does.
  // The cursor leaving the editor is what makes a held-back remote change safe
  // to apply — and it moved nothing the render decision reads, so nothing
  // re-evaluated it. `activeIsInEditor` is computed from
  // `document.activeElement`, which is not reactive; the only two things that
  // clear the banner both require the STORE to change again. So a change that
  // arrived while the cursor was parked in the body was refused once and then
  // never reconsidered: the note list showed the new content, the editor pane
  // kept the old, and the banner stayed up until the user switched notes.
  //
  // Measured with a mounted-component probe before fixing (2026-08-27): focus
  // outside → rendered on pass 3, banner never raised; focus inside → banner
  // raised on pass 3 and NO pass 4 ever ran. Then confirmed live on
  // icloud:kaiwan@me.com, twice.
  //
  // `tick()` first because focusout fires BEFORE focus settles, so reading
  // activeElement in the same turn still sees the editor.
  async function onEditorFocusOut() {
    await tick();
    externalReloadNonce += 1;
  }

  function reloadFromRemote() {
    clearTimeout(saveTimer);
    userEdited = false;
    if (editorEl && document.activeElement instanceof Node && editorEl.contains(document.activeElement)) {
      (document.activeElement as HTMLElement).blur();
    }
    lastPushedBody = lastRenderedBody;
    lastPushedTitle = title;
    externalReloadNonce += 1;
  }

  // Re-arm THIS note's push, so the worker tries it again on its next tick.
  //
  // One note, not the account (2026-09-30). The account-wide retry assumed a
  // refusal is rarely about the note — true of Microsoft's missing folder id,
  // false of iCloud's per-note refusals, where it unlocked notes the user was
  // not looking at until the worker refused each again. Other blocked notes
  // re-arm on their own "Try again", or on an edit or a move.
  //
  // Optimistic, per the local-first doctrine: clear the flag in the store
  // FIRST so the toolbar stops saying "not synced" the instant the button is
  // pressed, then send. On failure the flag goes back. If the cause is still
  // there, the worker re-blocks the row on its next tick and the message
  // returns on its own — which is the honest outcome, not a rollback.
  // The pin push was refused permanently (migration 23, gotcha #14): pinned
  // here, never on other devices. Independent of the content's own block.
  $: pinBlock = $selectedNote?.pinned
    ? pinBlockReason($syncBlocks, $selectedNote.account_id || $currentAccount, $selectedNote.uuid)
    : null;
  function retryPin() {
    const n = $selectedNote;
    const accountId = n?.account_id || $currentAccount;
    if (!n || !accountId) return;
    retryPinBlock(accountId, n.uuid).catch((e) => error.set(`Failed to retry pin sync: ${e}`));
  }

  async function retrySync() {
    const target = $selectedNote;
    const accountId = target?.account_id || $currentAccount;
    if (!target || !accountId) return;
    const key = { account_id: accountId, uuid: target.uuid };
    const before = { push_blocked_reason: target.push_blocked_reason, push_blocked_by_remote: target.push_blocked_by_remote };
    const cleared = (n: Note) => sameNote(n, key) ? { ...n, push_blocked_reason: null, push_blocked_by_remote: false } : n;
    notes.update((ns) => ns.map(cleared));
    selectedNote.update((n) => (n ? cleared(n) : n));
    // The refusal can also be held by the persisted state (syncBlock): clear
    // that too, or the toolbar would keep saying "Not synced" after the press.
    const pKey = noteKey(key);
    const persistedBefore = $persistenceByNote[pKey];
    if (persistedBefore?.push_blocked_reason) {
      persistenceByNote.update((m) => ({ ...m, [pKey]: { ...persistedBefore, push_blocked_reason: null, push_blocked_by_remote: false } }));
    }
    try {
      await invoke('retry_blocked_push', { accountId, uuid: target.uuid });
    } catch (e) {
      // Only if nothing newer replaced the note meanwhile — a later edit
      // re-arms it anyway, and a later block carries the current reason.
      const restore = (n: Note) => sameNote(n, key) && n.local_version === target.local_version && !n.push_blocked_reason
        ? { ...n, ...before } : n;
      notes.update(ns => ns.map(restore));
      selectedNote.update(n => n ? restore(n) : n);
      if (persistedBefore?.push_blocked_reason) {
        persistenceByNote.update((m) => {
          const now = m[pKey];
          return now && now.local_version === persistedBefore.local_version && !now.push_blocked_reason
            ? { ...m, [pKey]: persistedBefore } : m;
        });
      }
      error.set(String(e));
    }
  }

  // ─── Metadata line (spec 2026-10-20 §6.1) ───────────────────────────
  // Every prop of the line is derived from the open note in THIS one pass
  // (gotcha #28): the note it is about travels with its account prefix,
  // folder, status and tag count, so no element can show the previous note's
  // data for a render. The per-note states compete for the one indicator by
  // pickMetaStatus's priority; their sentences are the old banners' wording.
  //
  // The whole-account read-only state is NOT a status: it stays a visible
  // notice under the line (the spec's exception), and — as in the banner
  // chain it replaces — while it shows, the push-block statuses do not.
  $: meta = (() => {
    const n = $selectedNote;
    if (!n) return null;
    // The row's refusal, or the persisted one for its current version.
    const block = syncBlock(n, $persistenceByNote[noteKey(n)]);
    const reason = block?.reason;
    // Hidden while an edit is pending: that edit re-arms the push (gotcha
    // #14), so the old refusal is no longer the current one. If the push is
    // refused again, the worker's nudge brings it back.
    const editPending = userEdited || !!$pendingLocalEdits[persistenceKey] || $savingNoteKeys.has(persistenceKey);
    const status = pickMetaStatus([
      // The reason as TEXT (in the popover), not only the toolbar's tooltip:
      // it is the user's instruction ("open it in Apple Notes to edit").
      canWriteNotes && !!reason && !!block?.byRemote && {
        kind: 'readonly',
        label: 'Read-only',
        detail: `Read-only in Jodd — ${blockedReasonText(reason)}`,
      } satisfies MetaStatus,
      canWriteNotes && !!reason && !block?.byRemote && !editPending && {
        kind: 'notSynced',
        label: 'Not synced',
        detail: `Not synced — ${blockedReasonText(reason)}`,
        action: { label: 'Try again', run: retrySync, title: reason },
      } satisfies MetaStatus,
      !!trustChip && {
        kind: 'trust',
        label: trustLabel(trustChip),
        detail: trustSentence(trustChip),
        icon: trustChip.unrecorded_change ? 'pencil' : !trustChip.unreviewed_by ? 'refresh' : 'eye',
        note: trustChip.created_by ? `Created by ${trustChip.created_by}.` : undefined,
        action: {
          label: 'Mark reviewed',
          run: onMarkReviewed,
          disabled: reviewBlocked,
          title: reviewBlocked ? 'Load the latest version of this note first' : '',
        },
        error: trustError || null,
      } satisfies MetaStatus,
      // Independent of the content's own block: the content can be synced
      // while the pin is not. Text, not only NoteList's tooltip — Android has
      // no hover.
      !!pinBlock && {
        kind: 'pinBlocked',
        label: 'Pin not synced',
        detail: `Pin not synced — ${pinBlock}`,
        action: { label: 'Try again', run: retryPin },
      } satisfies MetaStatus,
    ]);
    return {
      note: n,
      accountLabel: accountPrefixOf($activeAccounts.find((a) => a.id === n.account_id), $activeAccounts),
      folderPath: n.label || 'Notes',
      status,
      tagCount: currentTags.length,
      tagsEditable: canEditTags && !n.uuid?.startsWith('tmp:'),
    };
  })();

  function revealFolder() {
    const lbl = $selectedNote?.label || 'Notes';
    const acct = $selectedNote?.account_id;
    if (acct && acct !== $currentAccount) currentAccount.set(acct);
    if ($searchQuery) searchQuery.set('');
    // Leaves a Smart Folder or tag filter too: either would keep the list.
    navigateToFolder(lbl);
    // Phone: App.svelte's folder watcher moves panes only when the
    // folder VALUE changes, and leaving a Smart Folder for the folder
    // underneath changes nothing there. Same reason as Sidebar's
    // selectFolder; a no-op off the phone layout. The title names the
    // list for the Back label of the note opened from it.
    navigateToPane('list', currentListTitle());
  }

  // The tags popover (TagsPopover, spec 2026-10-20 §6.2), opened from #N or
  // ⋯ › Tags…. Open for ONE note, as ⋯ is: `tagsFor` is the note it was
  // opened on, so a switch hides it in the same pass that changes the note
  // (gotcha #28), and it stays closed when that note comes back.
  let metaTagsBtn: HTMLButtonElement | null = null;
  let tagsAnchor: HTMLElement | null = null;
  let tagsOpen = false;
  let tagsFor = '';
  $: tagsShown = tagsOpen && !!$selectedNote && tagsFor === noteKey($selectedNote);
  function closeTagsOnSwitch(n: Note | null) {
    if (tagsOpen && (!n || noteKey(n) !== tagsFor)) tagsOpen = false;
  }
  $: closeTagsOnSwitch($selectedNote);
  function openTags(anchor: HTMLElement) {
    // #N again while it is open closes it (the Popover leaves its anchor's
    // own clicks alone).
    if (tagsShown && anchor === tagsAnchor) {
      tagsOpen = false;
      return;
    }
    tagsAnchor = anchor;
    tagsFor = $selectedNote ? noteKey($selectedNote) : '';
    tagsOpen = true;
  }
  // The sheet's edits land after it has closed: only on the note it was for.
  // On an editor already destroyed (its sheet's edits arrive after a
  // rotation swapped it out) they go to the live editor for that note.
  function addTagFromPopover(tag: string) {
    if (editorDestroyed) return void handOffTagEdits(tagEditReceiver, tagsFor, [{ op: 'add', tag }]);
    if ($selectedNote && noteKey($selectedNote) === tagsFor) addNormalizedTag(tag);
  }
  function removeTagFromPopover(tag: string) {
    if (editorDestroyed) return void handOffTagEdits(tagEditReceiver, tagsFor, [{ op: 'remove', tag }]);
    if ($selectedNote && noteKey($selectedNote) === tagsFor) removeTag(tag);
  }

  // Tag edits handed over from a sheet whose editor a rotation destroyed
  // (editor/tagEditHandoff.ts). This editor takes them only for the note it
  // shows, and applies them once that note's body is in editorEl — on a
  // freshly created editor that is after its first render.
  let editorDestroyed = false;
  let bodyRenderedKey: string | null = null;
  let handedOffTagEdits: { key: string; ops: TagEdit[] } | null = null;
  const tagEditReceiver: TagEditReceiver = {
    accept(key, ops) {
      if (editorDestroyed || !$selectedNote || noteKey($selectedNote) !== key) return false;
      handedOffTagEdits = { key, ops: [...(handedOffTagEdits?.key === key ? handedOffTagEdits.ops : []), ...ops] };
      applyHandedOffTagEdits();
      return true;
    },
  };
  function applyHandedOffTagEdits() {
    const h = handedOffTagEdits;
    if (!h || bodyRenderedKey === null) return;
    handedOffTagEdits = null;
    // R26: never on another note — the body rendered, or the note now
    // selected, being a different one drops them.
    if (bodyRenderedKey !== h.key || !$selectedNote || noteKey($selectedNote) !== h.key) return;
    for (const o of h.ops) (o.op === 'add' ? addNormalizedTag : removeTag)(o.tag);
  }
  const unregisterTagEditReceiver = registerTagEditReceiver(tagEditReceiver);
  onDestroy(() => {
    editorDestroyed = true;
    unregisterTagEditReceiver();
  });

  // ⋯ (NoteMoreMenu, spec 2026-10-20 §6.1): the toolbar's on desktop and
  // tablet; on a phone, where the toolbar row does not render (§5.6), the
  // note PaneHeader's — NoteActionsButton, which reaches this menu through
  // the noteActionsMenu store. One menu either way, wired to this editor's
  // handlers; only its anchor differs. Open for ONE note: `moreFor` is the
  // note it was opened on, and the menu shows only while that is still the
  // open note — a switch hides it in the same pass that changes the note
  // (gotcha #28), with no stale note's actions on screen.
  // A rotation closes it, on purpose: App mounts one editor per layout, so
  // the swap destroys the editor that owns the menu, and its sheet's history
  // entry goes with it — the next Back goes to the list. The tags sheet
  // closes the same way (noteActionsSheetRotation.test.ts).
  let moreBtn: HTMLButtonElement | undefined;
  let headerMoreBtn: HTMLElement | undefined;
  let moreOpen = false;
  let moreFor = '';
  $: moreShown = moreOpen && !!$selectedNote && moreFor === noteKey($selectedNote);
  // …and stays closed: switching back to the note it was opened on must not
  // reopen it by itself. (A call, so the statement depends on the note only.)
  function closeMoreOnSwitch(n: Note | null) {
    if (moreOpen && (!n || noteKey(n) !== moreFor)) moreOpen = false;
  }
  $: closeMoreOnSwitch($selectedNote);
  function toggleMore() {
    const key = $selectedNote ? noteKey($selectedNote) : '';
    moreOpen = !moreShown;
    moreFor = key;
  }
  // The header's ⋯ asks: it becomes the anchor in the same step as the open,
  // so the menu mounts on it.
  function toggleMoreFromHeader(anchor: HTMLElement) {
    headerMoreBtn = anchor;
    toggleMore();
  }
  $: moreAnchor = phoneLayout ? headerMoreBtn : moreBtn;
  const noteActionsPub = claimNoteActionsMenu();
  $: noteActionsPub.set(
    phoneLayout && $selectedNote ? { toggle: toggleMoreFromHeader, open: moreShown } : null,
  );
  onDestroy(() => noteActionsPub.release());

  // ── Toolbar overflow (Task 6.4, spec §5.6) ────────────────────────────────
  // One data array drives the toolbar row AND the More formatting menu, so the
  // buttons have a single implementation (the format* handlers below).
  type FmtId = 'bold' | 'italic' | 'underline' | 'strike' | 'link' | 'heading' | 'quote' | 'code' | 'bullets' | 'numbered' | 'task';
  type FmtItem = { id: FmtId; title: string; /** has an explicit aria-label (an icon, or a glyph that is not its name) */ aria: boolean; run: () => void };
  $: fmtItems = [
    { id: 'bold', title: 'Bold', aria: false, run: formatBold },
    { id: 'italic', title: 'Italic', aria: false, run: formatItalic },
    { id: 'underline', title: 'Underline', aria: false, run: formatUnderline },
    { id: 'strike', title: `Strikethrough (${modKey}+Shift+X)`, aria: false, run: formatStrike },
    { id: 'link', title: `Link (${modKey}+K)`, aria: true, run: formatLink },
    { id: 'heading', title: `Heading 2 (${modKey}+${altKey}+2)`, aria: false, run: formatHeading },
    { id: 'quote', title: `Blockquote (${modKey}+Shift+.)`, aria: true, run: formatBlockquote },
    { id: 'code', title: `Inline code (${modKey}+E)`, aria: false, run: formatInlineCode },
    { id: 'bullets', title: `Bullet List (${modKey}+Shift+8)`, aria: false, run: formatList },
    { id: 'numbered', title: `Numbered List (${modKey}+Shift+7)`, aria: false, run: formatOrderedList },
    { id: 'task', title: `Task / Checklist (${modKey}+Shift+9)`, aria: true, run: formatTask },
  ] as FmtItem[];

  // How many of fmtItems (a suffix) are in the menu instead of the row.
  let overflowN = 0;
  let moreFmtOpen = false;
  let moreFmtBtn: HTMLButtonElement | undefined;
  $: showMoreFmt = overflowN > 0;
  $: if (!showMoreFmt) moreFmtOpen = false;

  // The format applies to the selection (the item's mousedown kept it in the
  // editor); then the menu closes and typing resumes in the body.
  function runFmtFromMenu(run: () => void) {
    run();
    moreFmtOpen = false;
    editorEl?.focus();
  }

  // Measures the row and decides overflowN. Buttons hidden by overflow keep
  // their layout box (visibility, not display), so every width is real; a
  // toolbar that is not laid out reads 0 everywhere and is ignored rather than
  // taken as "everything fits" or "nothing does". The observer watches the
  // toolbar and its right side (the save text changes width); overflowN only
  // changes the actions block, which is not observed, so it cannot loop.
  let moreFmtWidth = 0;
  function watchToolbarOverflow(node: HTMLElement) {
    const px = (v: string) => parseFloat(v) || 0;
    const measure = () => {
      const buttons = Array.from(node.querySelectorAll<HTMLElement>('.toolbar-actions > .fmt-btn[data-fmt]'));
      const right = node.querySelector<HTMLElement>('.toolbar-right');
      if (!buttons.length || !right || node.clientWidth === 0) return;
      const raw = buttons.map((b) => b.offsetWidth);
      if (raw.every((w) => w === 0)) return;
      const cs = getComputedStyle(node);
      const actionsGap = px(getComputedStyle(buttons[0].parentElement as HTMLElement).columnGap);
      const widths = raw.map((w) => w + actionsGap);
      const more = node.querySelector<HTMLElement>('.more-fmt-btn');
      if (more && more.offsetWidth) moreFmtWidth = more.offsetWidth + actionsGap;
      const available = node.clientWidth - px(cs.paddingLeft) - px(cs.paddingRight);
      const reserved = right.offsetWidth + px(cs.columnGap);
      const { count } = overflowWithMore(available, widths, reserved, moreFmtWidth || Math.max(...widths));
      if (count !== overflowN) overflowN = count;
    };
    // No observer (jsdom): the row simply never overflows.
    const ro = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(measure);
    ro?.observe(node);
    const right = node.querySelector('.toolbar-right');
    if (right) ro?.observe(right);
    measure();
    void document.fonts?.ready?.then(measure);
    return { destroy: () => ro?.disconnect() };
  }

  // formatting commands
  function formatUndo() { document.execCommand('undo'); }
  function formatRedo() { document.execCommand('redo'); }
  function formatBold() { document.execCommand('bold'); }
  function formatItalic() { document.execCommand('italic'); }
  function formatUnderline() { document.execCommand('underline'); }
  function formatStrike() { document.execCommand('strikeThrough'); }
  function formatList() { document.execCommand('insertUnorderedList'); }
  function formatOrderedList() { document.execCommand('insertOrderedList'); }
  // Heading via formatBlock. <h2> round-trips as plain HTML Apple preserves in
  // the body; toggle off by reverting an existing h2 line back to a div.
  function formatHeading() {
    const block = (document.queryCommandValue('formatBlock') || '').toLowerCase();
    document.execCommand('formatBlock', false, block === 'h2' ? '<div>' : '<h2>');
  }

  // Turn the caret's line into a task: a checkbox + space at its start. The
  // trailing &nbsp; gives the cursor a place to land when the user starts
  // typing — without it, the caret falls behind the input.
  //
  // `contenteditable="false"` on the input itself is critical: inputs inside
  // a contenteditable region are otherwise treated as text-flow content and
  // some browsers swallow clicks for caret placement instead of toggling the
  // form control. insertChecklist sets it after inserting — in the inserted
  // markup it corrupts undo (../editor/bodyEdits.ts).
  function formatTask() {
    if (!editorEl) return;
    insertChecklist(editorEl);
    onInput();
  }

  // New first-class formatting: link, heading levels, blockquote, code.
  // Link prompt — Tauri 2's WKWebView silently suppresses window.prompt(), so
  // we render a small in-editor popover instead. The selection is captured
  // BEFORE the popover steals focus (moving focus to the popover's input
  // collapses the contenteditable selection), then restored on submit.
  let linkPromptOpen = false;
  let linkPromptUrl = '';
  let linkPromptText = '';
  let linkPromptHasSelection = false;
  let linkPromptSavedRange: Range | null = null;
  let linkPromptUrlEl: HTMLInputElement | null = null;
  // Edit link (the chip's button): the <a> the prompt rewrites. Null when the
  // prompt makes a new link.
  let linkPromptEditing: HTMLAnchorElement | null = null;
  // The anchor's text when the edit began: unchanged text means a URL-only edit.
  let linkPromptEditingText = '';
  function formatLink() {
    linkPromptEditing = null;
    const sel = window.getSelection();
    if (sel && sel.rangeCount > 0) {
      linkPromptSavedRange = sel.getRangeAt(0).cloneRange();
      linkPromptHasSelection = sel.toString().length > 0;
      linkPromptText = linkPromptHasSelection ? sel.toString() : '';
    } else {
      linkPromptSavedRange = null;
      linkPromptHasSelection = false;
      linkPromptText = '';
    }
    linkPromptUrl = 'https://';
    linkPromptOpen = true;
    focusLinkPrompt();
  }
  function focusLinkPrompt() {
    // setTimeout 0 lets Svelte mount the input before we focus it.
    setTimeout(() => {
      linkPromptUrlEl?.focus();
      linkPromptUrlEl?.select();
    }, 0);
  }
  /** The same prompt, prefilled from an existing link; OK replaces that link. */
  function editLinkPrompt(a: HTMLAnchorElement) {
    if (!editorEl?.contains(a)) return;
    linkPromptEditing = a;
    linkPromptSavedRange = null;
    linkPromptHasSelection = false; // show the text field: the label is editable too
    linkPromptText = linkPromptEditingText = a.textContent ?? '';
    linkPromptUrl = a.getAttribute('href') ?? '';
    linkPromptOpen = true;
    focusLinkPrompt();
  }
  /**
   * Replace `a` with the edited link in ONE edit command, so one Undo restores
   * it. A URL-only edit keeps everything else: the anchor is cloned and only
   * its href changes, so <b> inside it, a linked <img>, its spacing and its
   * other attributes survive. Only a changed label rebuilds the link from the
   * typed text, as typed (never trimmed: "link " + "now" must not merge).
   */
  function replaceLink(a: HTMLAnchorElement, url: string) {
    const text = linkPromptText;
    const textChanged = text !== linkPromptEditingText;
    closeLinkPrompt();
    const sel = window.getSelection();
    // The body may have been replaced under the prompt (a note switch, a
    // re-render): then there is nothing left to edit.
    if (!sel || !editorEl?.contains(a)) return;
    const range = document.createRange();
    range.selectNode(a);
    sel.removeAllRanges();
    sel.addRange(range);
    // As removeLink: the webview's own `input` event already ran onInput.
    let html: string;
    if (textChanged) {
      html = `<a href="${escapeAttr(url)}">${escapeText(text.trim() ? text : url)}</a>`;
    } else {
      const clone = a.cloneNode(true) as HTMLAnchorElement;
      clone.setAttribute('href', url);
      html = clone.outerHTML;
    }
    const generation = editGeneration;
    document.execCommand('insertHTML', false, html);
    if (editGeneration === generation) onInput();
  }
  function applyLinkPrompt() {
    const url = linkPromptUrl.trim();
    if (!url || url === 'https://') { closeLinkPrompt(); return; }
    if (linkPromptEditing) { replaceLink(linkPromptEditing, url); return; }
    // Restore the saved selection so createLink / insertHTML targets the
    // original caret position instead of the popover input.
    if (linkPromptSavedRange) {
      const sel = window.getSelection();
      if (sel) { sel.removeAllRanges(); sel.addRange(linkPromptSavedRange); }
    }
    if (linkPromptHasSelection) {
      document.execCommand('createLink', false, url);
    } else {
      const label = (linkPromptText || url).trim();
      document.execCommand(
        'insertHTML',
        false,
        `<a href="${escapeAttr(url)}">${escapeText(label)}</a>`,
      );
    }
    closeLinkPrompt();
    onInput();
  }
  function closeLinkPrompt() {
    linkPromptOpen = false;
    linkPromptSavedRange = null;
    linkPromptEditing = null;
    linkPromptEditingText = '';
  }
  function onLinkPromptKey(e: KeyboardEvent) {
    if (e.key === 'Enter') { e.preventDefault(); applyLinkPrompt(); }
    else if (e.key === 'Escape') { e.preventDefault(); closeLinkPrompt(); }
  }
  function formatHeadingLevel(n: 1 | 2 | 3) {
    const cur = (document.queryCommandValue('formatBlock') || '').toLowerCase();
    const tag = `h${n}`;
    document.execCommand('formatBlock', false, cur === tag ? '<div>' : `<${tag}>`);
    onInput();
  }
  function formatBlockquote() {
    const cur = (document.queryCommandValue('formatBlock') || '').toLowerCase();
    document.execCommand('formatBlock', false, cur === 'blockquote' ? '<div>' : '<blockquote>');
    onInput();
  }
  function formatInlineCode() {
    const sel = window.getSelection();
    const text = sel?.toString() || '';
    // Multi-line selection: inline <code> would replace the line breaks
    // (execCommand insertHTML deletes the entire selected range as a unit),
    // collapsing N lines into 1. That's never what the user wants. Promote to
    // a block-level <pre><code>…</code></pre> which preserves the newlines.
    if (text.includes('\n')) {
      document.execCommand(
        'insertHTML',
        false,
        `<pre><code>${escapeText(text)}</code></pre>`,
      );
      onInput();
      return;
    }
    if (text) {
      document.execCommand('insertHTML', false, `<code>${escapeText(text)}</code>`);
    } else {
      // Zero-width space gives the caret a place to land inside the empty <code>.
      document.execCommand('insertHTML', false, '<code>​</code>');
    }
    onInput();
  }
  function formatCodeBlock() {
    const cur = (document.queryCommandValue('formatBlock') || '').toLowerCase();
    document.execCommand('formatBlock', false, cur === 'pre' ? '<div>' : '<pre>');
    onInput();
  }
  // ─── Phone format bar (spec 2026-10-20 §5.6) ──────────────────────────────
  // On a phone the toolbar row does not render (it wrapped onto 2–3 rows of
  // ~22px buttons); FormatBar sits above the soft keyboard instead, while the
  // title or the body has focus. Its commands are the toolbar's handlers
  // above — formatting has one implementation.
  $: phoneLayout = $isAndroid && $androidLayoutMode === 'phone';
  let titleEl: HTMLTextAreaElement | undefined;
  // Is focus in the title or the body? From the focus events' own targets:
  // during focusout document.activeElement is already <body>, and the new
  // owner is relatedTarget.
  let editingFocused = false;
  let titleFocused = false;
  function isEditingTarget(t: EventTarget | null): boolean {
    return t instanceof Node && (t === titleEl || (!!editorEl && editorEl.contains(t)));
  }
  function setEditingFocus(t: EventTarget | null) {
    editingFocused = isEditingTarget(t);
    titleFocused = !!t && t === titleEl;
  }
  function onEditingFocusIn(e: FocusEvent) {
    setEditingFocus(e.target);
    keyboardDismissed = false;
  }
  function onEditingFocusOut(e: FocusEvent) {
    setEditingFocus(e.relatedTarget);
    // The # list belongs to the body's caret: gone once focus leaves the body
    // (the title included). Its own rows never take focus.
    const to = e.relatedTarget;
    if (!(to instanceof Node && editorEl?.contains(to))) closeHashtagPicker();
  }
  // A body re-render ({#key editorRenderKey}) replaces the focused element
  // without a focusout: re-read focus once the new element is in.
  function refreshEditingFocus(_key: number) {
    void tick().then(() => setEditingFocus(document.activeElement));
  }
  $: refreshEditingFocus(editorRenderKey);
  // Android Back hides the keyboard without blurring the body, and the bar
  // stayed behind at the very bottom, over the navigation bar's buttons (live
  // pass 2026-10-21). The keyboard going away (its inset falling to 0) hides
  // the bar like ⌄ does; the keyboard coming back, or focus arriving again,
  // shows it. A device that never reports an inset never hides it this way —
  // FormatBar then clears the navigation bar itself.
  let keyboardDismissed = false;
  let lastKeyboardInset = 0;
  function trackKeyboard(px: number) {
    if (px > 0) keyboardDismissed = false;
    else if (lastKeyboardInset > 0) keyboardDismissed = true;
    lastKeyboardInset = px;
  }
  $: trackKeyboard($keyboardInset);
  $: formatBarShown = phoneLayout && editable && editingFocused && !keyboardDismissed;
  // Every command but Hide keyboard acts on the body: from the title, `#` and
  // `[[` would type into the title and inline code would insert a zero-width
  // space there. They are off while the title has focus.
  const TITLE_DISABLED: readonly FormatCommand[] = FORMAT_COMMANDS.filter((c) => c !== 'dismiss');
  // `#` is also off where a tag is an object Jodd cannot create (iCloud,
  // gotcha #38): it would type `#` text that Apple never treats as a tag, and
  // the tag list it exists to open is off there too.
  const NO_HASHTAG: readonly FormatCommand[] = ['hashtag'];
  $: formatBarDisabled = titleFocused ? TITLE_DISABLED : tagsEditable(editorTagModel) ? [] : NO_HASHTAG;

  // Aa: the text-style sheet. The sheet is a modal <dialog>, so the editor
  // behind it is inert while it is up (focus refused, execCommand false — see
  // ruling R26): the caret is remembered as it opens, and the pick is applied
  // once the sheet has gone, against the note it was opened on.
  const TEXT_STYLES = [
    { label: 'Title', tag: 'h1' },
    { label: 'Heading', tag: 'h2' },
    { label: 'Subheading', tag: 'h3' },
    { label: 'Body', tag: 'div' },
  ] as const;
  let textStyleOpen = false;
  let textStyleModal: Modal | undefined;
  let textStyleRange: Range | null = null;
  let textStyleFor = '';
  let textStylePick: string | null = null;
  let textStyleCurrent = '';
  function bodyRange(): Range | null {
    const sel = window.getSelection();
    if (!editorEl || !sel || sel.rangeCount === 0) return null;
    const r = sel.getRangeAt(0);
    return editorEl.contains(r.startContainer) ? r.cloneRange() : null;
  }
  function currentBlockTag(): string {
    try {
      const v = (document.queryCommandValue?.('formatBlock') || '').toLowerCase();
      return /^h[1-3]$/.test(v) ? v : 'div';
    } catch {
      return 'div';
    }
  }
  function openTextStyle() {
    // A style is a body line's: from the title there is nothing to style.
    const r = bodyRange();
    if (!r || !$selectedNote) return;
    textStyleRange = r;
    textStyleFor = noteKey($selectedNote);
    textStylePick = null;
    textStyleCurrent = currentBlockTag();
    textStyleOpen = true;
  }
  function pickTextStyle(tag: string) {
    textStylePick = tag;
    void textStyleModal?.requestClose();
  }
  function onTextStyleClose() {
    textStyleOpen = false;
    const range = textStyleRange;
    const tag = textStylePick;
    const forNote = textStyleFor;
    textStyleRange = null;
    textStylePick = null;
    void tick().then(() => {
      const n = get(selectedNote);
      if (!range || !editorEl || !n || noteKey(n) !== forNote || !editorEl.contains(range.startContainer)) return;
      editorEl.focus();
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(range);
      if (!tag) return;
      document.execCommand('formatBlock', false, `<${tag}>`);
      onInput();
    });
  }

  // `#` and `[[` type their trigger at the caret, as an edit command (so it
  // undoes); the input event that fires is what opens the pickers (onInput →
  // detectLinkPicker, detectHashtagPicker).
  function insertAtCaret(text: string) {
    document.execCommand('insertText', false, text);
  }
  // `#` right after a word would not start a tag (hashtagQueryAtCaret), so
  // the button puts a space before it there — a tap on # always opens the
  // tag list.
  function insertHashtagTrigger() {
    const r = bodyRange();
    insertAtCaret(r && r.collapsed && /[\p{L}\p{N}\p{M}_\-]$/u.test(textBeforeCaret(r)) ? ' #' : '#');
  }
  // The text the caret follows. A caret on an element boundary (after a tap,
  // or a selection set by script) types into the end of the node before it —
  // measured in Chrome: at (body, 2) after `<div>last line</div>` the `#`
  // landed as `last line#` — so read that node's deepest last text.
  function textBeforeCaret(r: Range): string {
    const n = r.startContainer;
    if (n.nodeType === 3) return (n as Text).data.slice(0, r.startOffset);
    let c: Node | null = r.startOffset > 0 ? n.childNodes[r.startOffset - 1] : null;
    while (c && c.nodeType === 1 && c.lastChild) c = c.lastChild;
    return c && c.nodeType === 3 ? (c as Text).data : '';
  }
  function dismissKeyboard() {
    const a = document.activeElement;
    if (a instanceof HTMLElement && isEditingTarget(a)) a.blur();
  }
  const formatCommands: Record<FormatCommand, () => void> = {
    heading: openTextStyle,
    bold: formatBold,
    italic: formatItalic,
    underline: formatUnderline,
    strike: formatStrike,
    bullet: formatList,
    numbered: formatOrderedList,
    task: formatTask,
    quote: formatBlockquote,
    code: formatInlineCode,
    link: formatLink,
    hashtag: insertHashtagTrigger,
    wikilink: () => insertAtCaret('[['),
    dismiss: dismissKeyboard,
  };

  function escapeText(s: string): string {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }
  function escapeAttr(s: string): string {
    return escapeText(s).replace(/"/g, '&quot;');
  }

  // Caret-in-code: <pre> / <code> contexts where markdown triggers must NOT fire
  // (they would mangle the user's literal text) and where paste must always be
  // text/plain (so a pasted snippet doesn't get its angle-brackets interpreted).
  function caretInCodeContext(): boolean {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0) return false;
    const node = sel.getRangeAt(0).startContainer;
    const el = (node.nodeType === 3 ? node.parentElement : node) as HTMLElement | null;
    return !!el?.closest('pre, code');
  }

  // Markdown-style typing triggers (`# `, `- `, `1. `, `> `). Matching, the
  // live-keystroke gate and the execCommand-only DOM changes live in
  // ../editor/markdownTriggers.ts. The trigger just applied is kept here so a
  // Backspace as the very next keystroke (onEditorKeydown) reverts it instead
  // of deleting a character.
  let lastTrigger: AppliedTrigger | null = null;

  // Cheatsheet modal — surfaces every shortcut + the markdown triggers so they
  // are not hidden behaviour. See the toolbar ⓘ button.
  let showCheatsheet = false;
  // What the cheatsheet and toolbar tooltips call the modifiers — the keys
  // isShortcutMod actually accepts on this platform.
  $: modKey = shortcutModLabel($isMacos);
  $: altKey = $isMacos ? 'Option' : 'Alt';
  // Diagnostic — visible next to the "Saved" indicator in the toolbar.
  // Remove once markdown triggers are confirmed working.

  // Sanitize pasted rich HTML so foreign font sizes / colors / fonts do not leak
  // into the note. Spotlight, Quick Look, and many web pages put inline
  // style="font-size:..." on the clipboard HTML, which the default contenteditable
  // paste would preserve — pasted text then appears much bigger than the
  // surrounding line. We keep structural tags (links, lists, headings, basic
  // emphasis) so pastes from articles still carry meaning; we drop presentation.
  function onEditorPaste(e: ClipboardEvent) {
    const cd = e.clipboardData;
    if (!cd) return;
    e.preventDefault();
    const html = cd.getData('text/html');
    const text = cd.getData('text/plain');
    // Inside <pre>/<code>, ALWAYS paste as plain text. Code blocks are for
    // literal source — sanitizing a syntax-highlighted clipboard would let
    // DOMParser interpret pasted <b>foo</b> as an actual bold tag.
    if (caretInCodeContext()) {
      document.execCommand('insertText', false, text);
      onInput();
      return;
    }
    // No HTML on the clipboard (a URL from a browser's address bar): not
    // insertText, which would fold consecutive pastes into one undo step.
    if (!html) {
      if (pastePlainText(text)) onInput();
      return;
    }
    const cleaned = sanitizePastedHtml(html);
    document.execCommand('insertHTML', false, cleaned);
    onInput();
  }

  // Whitelist of structural tags worth preserving across a paste. Anything else
  // is unwrapped (children kept, tag dropped). <font> and <span> are dropped
  // because they almost always carry presentational styling we don't want.
  const PASTE_TAG_WHITELIST = new Set([
    'A', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'CODE', 'PRE',
    'BR', 'P', 'DIV', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6',
    'UL', 'OL', 'LI', 'BLOCKQUOTE',
  ]);

  function sanitizePastedHtml(html: string): string {
    const doc = new DOMParser().parseFromString(html, 'text/html');
    // Process body's children — never the body itself, or it would be unwrapped
    // out of the document and doc.body would be null on read-back.
    const walk = (node: Node) => {
      const kids = Array.from(node.childNodes);
      for (const child of kids) walk(child);
      if (node.nodeType !== Node.ELEMENT_NODE) return;
      const el = node as Element;
      if (el.tagName === 'SCRIPT' || el.tagName === 'STYLE' || el.tagName === 'META') {
        el.remove();
        return;
      }
      if (!PASTE_TAG_WHITELIST.has(el.tagName)) {
        const parent = el.parentNode;
        if (!parent) return;
        while (el.firstChild) parent.insertBefore(el.firstChild, el);
        parent.removeChild(el);
        return;
      }
      for (const attr of Array.from(el.attributes)) {
        if (el.tagName === 'A' && attr.name === 'href') continue;
        el.removeAttribute(attr.name);
      }
    };
    for (const child of Array.from(doc.body.childNodes)) walk(child);
    return doc.body.innerHTML;
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
    if (e.button === 0 && onLinkClick(e)) return;
    detectLinkPicker(); // clicking elsewhere closes a stale [[ picker
    refreshOpenHashtagPicker(); // … and a stale # one (never opens one)
    const t = e.target as HTMLElement;
    if (t.tagName === 'INPUT' && (t as HTMLInputElement).type === 'checkbox') {
      // Defer to a microtask so the native click has already flipped
      // .checked. Reading the property at that point tells us the new state.
      queueMicrotask(() => {
        const cb = t as HTMLInputElement;
        setCheckboxState(cb, cb.checked);
        propagateChecklist(cb); // finishing all subtasks ticks the parent (& vice-versa)
        onInput();
      });
    }
  }

  // ─── Links (spec 2026-10-22-hypermedia-links-design.md) ─────────────────
  // Every link in the body — `<a>`, plain-text URL, `[[wikilink]]`,
  // `#meeting-…` anchor — is found by links/scan.ts, painted by links/paint.ts
  // with CSS highlights and opened through links/handlers.ts. No markup is
  // ever added to the note: the hint and cursor go on the editor element
  // itself (its innerHTML is what syncs to Apple), the paint is a highlight.
  //
  // A plain click puts the caret where it landed and opens the chip under the
  // link; the shortcut modifier + click (⌘ on macOS, Ctrl elsewhere —
  // `isShortcutMod`) follows it at once. tauri-plugin-opener also listens for
  // link clicks on `window` and opens an <a> on Ctrl/Shift+click unless the
  // click was already default-prevented, so EVERY click on an <a> is.
  type LinkPreview = { title: string; firstLine: string } | 'missing';
  // `locate` re-measures the link where it is NOW: the chip follows it when
  // the body scrolls or the keyboard moves the layout, and closes when it is
  // gone (null: the element left the body, or the text there changed).
  type LinkLocator = () => DOMRect | null;
  type LinkChipState = { target: LinkTarget; preview: LinkPreview | null; style: string; locate: LinkLocator };
  type LinkHit = { anchor: HTMLAnchorElement | null; target: LinkTarget | null; rect: DOMRect; locate: LinkLocator };
  let linkChip: LinkChipState | null = null;
  let linkChipLayerEl: HTMLDivElement | undefined;
  // The last scan, as offsets (gotcha #34: a re-render kills Ranges). Only
  // spans outside an <a>: the anchor is the link, found by closest('a').
  let linkText: LinkText | null = null;
  let linkSpans: LinkSpan[] = [];
  let linkScanPending = false;
  let linkScanFrame = 0;
  const linkPainter = createLinkPainter();
  // A note link's preview by wikilink key — the hover hint's title and the
  // chip's — resolved once per key and forgotten on a note switch.
  let linkPreviews = new Map<string, LinkPreview>();
  let linkPreviewsInFlight = new Set<string>();
  // Bumped on every note switch (same branch as citeSeq/connSeq, gotcha #28):
  // a resolve that answers after it is dropped.
  let linkEpoch = 0;
  // Only the latest open of a note link may act on its reply (as connSeq).
  let linkOpenSeq = 0;
  // Where the chip goes if a note link turns out to be missing (ruling: a
  // modifier-click on a missing note opens the chip, never anything else).
  let linkOpenAt: LinkLocator | null = null;

  /** At most one scan + paint per animation frame, however many callers. */
  function scheduleLinkScan() {
    if (linkScanPending) return;
    linkScanPending = true;
    linkScanFrame = requestAnimationFrame(() => {
      linkScanPending = false;
      runLinkScan();
    });
  }
  function runLinkScan() {
    if (!editorEl) {
      linkText = null;
      linkSpans = [];
      return;
    }
    const lt = buildLinkText(editorEl);
    const spans = scanLinks(lt).filter((s) => !s.insideAnchor);
    const ranges: Range[] = [];
    for (const s of spans) {
      const r = spanToRange(lt, s.start, s.end);
      if (r) ranges.push(r);
    }
    linkText = lt;
    linkSpans = spans;
    linkPainter.paint(ranges);
  }

  // A fresh editor element ({#key editorRenderKey}) has nothing scanned yet,
  // and a chip placed against the old one points at nothing.
  let linkScannedEditor: HTMLElement | null = null;
  function linkEditorReplaced(el: HTMLElement | null | undefined) {
    if ((el ?? null) === linkScannedEditor) return;
    linkScannedEditor = el ?? null;
    closeLinkChip();
    if (el) scheduleLinkScan();
  }
  $: linkEditorReplaced(editorEl);

  const pointIn = (r: DOMRect, x: number, y: number) => x >= r.left && x < r.right && y >= r.top && y < r.bottom;
  /** Which of a (wrapped) link's line boxes the point is in: -1 for none. */
  function rectIndexAt(target: Element | Range, x: number, y: number): number {
    return Array.from(target.getClientRects()).findIndex((r) => pointIn(r, x, y));
  }
  /** That line box again, wherever it is now (the whole box if it no longer exists). */
  function rectOf(target: Element | Range, index: number): DOMRect {
    return (index >= 0 ? target.getClientRects()[index] : undefined) ?? target.getBoundingClientRect();
  }

  /** The link under the pointer, if any. An <a> with a refused href (javascript:,
   *  relative…) is still reported, with a null target, so its click is prevented. */
  function linkAt(e: MouseEvent): LinkHit | null {
    if (!editorEl) return null;
    const node = e.target as Node | null;
    const el = node instanceof Element ? node : node?.parentElement ?? null;
    const a = el?.closest('a');
    if (a instanceof HTMLAnchorElement && editorEl.contains(a)) {
      return anchorHit(a, rectIndexAt(a, e.clientX, e.clientY));
    }
    // A scan of a replaced element is stale: no hits until the rescan lands.
    const lt = linkText;
    if (!lt || lt.root !== editorEl) return null;
    const span = hitTest(e.clientX, e.clientY, linkSpans, (s) => spanToRange(lt, s.start, s.end));
    if (!span) return null;
    return spanHit(lt, span, rectIndexAt(spanToRange(lt, span.start, span.end)!, e.clientX, e.clientY));
  }
  /** An <a> as a hit; `index` is the line box to measure (-1: the whole link). */
  function anchorHit(a: HTMLAnchorElement, index: number): LinkHit {
    return {
      anchor: a,
      target: targetFromHref(a.getAttribute('href') ?? '', a),
      rect: rectOf(a, index),
      locate: () => (editorEl && editorEl.contains(a) ? rectOf(a, index) : null),
    };
  }
  function spanHit(lt: LinkText, span: LinkSpan, index: number): LinkHit {
    const range = spanToRange(lt, span.start, span.end)!;
    const token = range.toString();
    return {
      anchor: null,
      target: targetFromSpan(span),
      rect: rectOf(range, index),
      // By the scan's offsets, so the Range is rebuilt each time; null once the
      // element was replaced or the text there is no longer this link.
      locate: () => {
        if (!editorEl || lt.root !== editorEl) return null;
        try {
          const r = spanToRange(lt, span.start, span.end);
          return r && editorEl.contains(r.startContainer) && r.toString() === token ? rectOf(r, index) : null;
        } catch {
          return null; // an offset past a node's end: the text changed
        }
      },
    };
  }

  /**
   * The link the caret is in, for Mod+Enter. "In" means the character just
   * before a collapsed caret is the link's: strictly inside it or right at its
   * end (where typing a URL leaves the caret, and just past an </a>). At its
   * start the caret is before the link, not in it. A selection is never in a
   * link. An <a> whose href is refused (javascript: …) is not a link here.
   */
  function linkAtCaret(): LinkHit | null {
    const sel = window.getSelection();
    if (!editorEl || !sel || sel.rangeCount === 0 || !sel.isCollapsed) return null;
    const { startContainer: node, startOffset: offset } = sel.getRangeAt(0);
    if (!editorEl.contains(node)) return null;
    // This keystroke's text, not the last frame's: a scan may still be pending.
    cancelAnimationFrame(linkScanFrame);
    linkScanPending = false;
    runLinkScan();
    const lt = linkText;
    if (!lt) return null;
    const p = linkTextOffset(lt, node, offset);
    if (p === null) return null;
    // The text node holding the character before the caret (at a node
    // boundary, the earlier one), and the <a> it sits in, if any.
    const before = lt.nodes.find((n) => n.start < p && p <= n.start + n.node.data.length);
    const a = before?.node.parentElement?.closest('a');
    // No pointer to pick a line box with: the chip, if any, goes under the first.
    if (a instanceof HTMLAnchorElement && editorEl.contains(a)) {
      const hit = anchorHit(a, 0);
      return hit.target ? hit : null;
    }
    const span = linkSpans.find((s) => s.start < p && p <= s.end);
    return span ? spanHit(lt, span, 0) : null;
  }
  /** A DOM point → its offset in the link text; null when no text is next to it. */
  function linkTextOffset(lt: LinkText, node: Node, offset: number): number | null {
    if (node.nodeType === Node.TEXT_NODE) {
      const n = lt.nodes.find((x) => x.node === node);
      return n ? n.start + offset : null;
    }
    // Between children: the end of the inline text just before the point.
    const prev = node.childNodes[offset - 1];
    const inline = prev && (prev.nodeType === Node.TEXT_NODE || (prev instanceof Element && INLINE_TAGS.has(prev.localName.toLowerCase())));
    const last = inline ? lastTextIn(prev) : null;
    const n = last ? lt.nodes.find((x) => x.node === last) : undefined;
    return n ? n.start + n.node.data.length : null;
  }
  function lastTextIn(n: Node): Text | null {
    if (n.nodeType === Node.TEXT_NODE) return n as Text;
    for (let c = n.lastChild; c; c = c.previousSibling) {
      const t = lastTextIn(c);
      if (t) return t;
    }
    return null;
  }

  /**
   * Follow a link by the modifier (click or Enter). A link the modifier may
   * not open — inert, or a named-app scheme — shows its chip instead of
   * nothing, so the gesture always answers.
   */
  function openByModifier(t: LinkTarget, hit: LinkHit) {
    linkOpenAt = hit.locate;
    if (!primaryOpen(t, linkCtx, 'modifier')) openLinkChip(t, hit.rect, hit.locate);
  }
  /** Mod+Enter: true when the caret was in a link and it was handled. */
  function openLinkAtCaret(): boolean {
    const hit = linkAtCaret();
    if (!hit?.target) return false;
    closeLinkChip();
    openByModifier(hit.target, hit);
    return true;
  }

  /** Returns true when the click was a link's and nothing else should run. */
  function onLinkClick(e: MouseEvent): boolean {
    const hit = linkAt(e);
    if (hit?.anchor) e.preventDefault();
    closeLinkChip();
    if (!hit?.target) return false;
    if (isShortcutMod(e, $isMacos)) {
      openByModifier(hit.target, hit);
      return true;
    }
    // Shift/Alt+click extend or adjust the selection; a drag that ends on a
    // link selected text. Neither is a request for the chip.
    if (e.shiftKey || e.altKey || e.metaKey || e.ctrlKey) return false;
    const sel = window.getSelection();
    if (sel && sel.rangeCount > 0 && !sel.isCollapsed) return false;
    openLinkChip(hit.target, hit.rect, hit.locate);
    return false; // the caret moved: the pickers below still re-read it
  }

  const linkChipStyle = (rect: DOMRect) => `${linkPickerStyle(rect)}; pointer-events: auto`;
  function openLinkChip(t: LinkTarget, rect: DOMRect, locate: LinkLocator, preview?: LinkPreview) {
    linkOpenAt = locate;
    const known = preview ?? (t.kind === 'note' && t.key ? linkPreviews.get(t.key) ?? null : null);
    linkChip = { target: t, preview: known, style: linkChipStyle(rect), locate };
    if (t.kind === 'note' && t.key && known === null) requestLinkPreview(t.key);
  }
  // The part of the body the user can see: the editor's box, less what the
  // phone's format bar and keyboard cover (the same inset caretListPosition uses).
  function editorVisibleBox(): { top: number; bottom: number; left: number; right: number } | null {
    if (!editorEl) return null;
    const box = editorEl.getBoundingClientRect();
    const covered = phoneLayout ? (formatBarShown ? FORMAT_BAR_HEIGHT : 0) + get(keyboardInset) : 0;
    const bottom = covered > 0 ? Math.min(box.bottom, window.innerHeight - covered) : box.bottom;
    return { top: box.top, bottom, left: box.left, right: box.right };
  }
  /**
   * Follow the link: on Android the tap that opened the chip also focuses the
   * body, the keyboard and format bar come up and the WebView scrolls the
   * caret into view — a scroll that lands AFTER the chip opened, and the chip
   * is the only way to open a link on a phone. So a scroll or a keyboard
   * change re-places the chip with the same placement as at open, and it
   * closes only when the link is entirely out of sight or no longer there.
   */
  function repositionLinkChip() {
    const chip = linkChip;
    if (!chip) return;
    const rect = chip.locate();
    const box = editorVisibleBox();
    if (!rect || !box || rect.bottom <= box.top || rect.top >= box.bottom || rect.right <= box.left || rect.left >= box.right) {
      closeLinkChip();
      return;
    }
    const style = linkChipStyle(rect);
    if (style !== chip.style) linkChip = { ...chip, style };
  }
  function linkChipLayoutChanged(_inset: number, _bar: boolean, _phone: boolean) {
    void tick().then(repositionLinkChip); // after the layout the change implies
  }
  $: linkChipLayoutChanged($keyboardInset, formatBarShown, phoneLayout);
  function closeLinkChip() {
    if (linkChip) linkChip = null;
  }

  type ResolvedLink = { note: Note; firstLine: string };
  const previewOf = (r: ResolvedLink | null): LinkPreview =>
    r ? { title: r.note.title || 'Untitled', firstLine: r.firstLine } : 'missing';
  function linkAccountId(): string | null {
    return get(selectedNote)?.account_id ?? get(currentAccount) ?? null;
  }

  /** Fill the preview for `key` (hover hint, chip), once per key per note open. */
  async function requestLinkPreview(key: string) {
    const accountId = linkAccountId();
    if (!accountId || linkPreviews.has(key) || linkPreviewsInFlight.has(key)) return;
    const epoch = linkEpoch;
    const inFlight = linkPreviewsInFlight;
    inFlight.add(key);
    try {
      const res = await invoke<ResolvedLink | null>('resolve_note_link', { accountId, key });
      if (epoch !== linkEpoch) return; // another note is open now
      const p = previewOf(res);
      linkPreviews.set(key, p);
      if (linkChip?.target.kind === 'note' && linkChip.target.key === key && linkChip.preview === null) {
        linkChip = { ...linkChip, preview: p };
      }
    } catch (err) {
      console.warn('[links] resolve_note_link (preview) failed', err);
    } finally {
      inFlight.delete(key);
    }
  }

  /** `ctx.openNote`: resolve from SQLite, then open it as the connections list does. */
  async function openNoteLink(t: LinkTarget): Promise<void> {
    const from = get(selectedNote);
    const accountId = linkAccountId();
    if (!t.key || !from || !accountId) return;
    const key = t.key;
    const locate = linkOpenAt;
    const seq = ++linkOpenSeq;
    const epoch = linkEpoch;
    const current = () => seq === linkOpenSeq && epoch === linkEpoch && sameNote(get(selectedNote), from);
    let res: ResolvedLink | null;
    try {
      res = await invoke<ResolvedLink | null>('resolve_note_link', { accountId, key });
    } catch (err) {
      console.warn('[links] resolve_note_link failed', err);
      if (current()) error.set("Couldn't open the linked note.");
      return;
    }
    // Local-first: the reply is dropped unless the same note is still open.
    if (!current()) return;
    linkPreviews.set(key, previewOf(res));
    if (!res) {
      // "No note called …" + Copy, where the link is now — if it still is.
      const rect = locate?.();
      if (locate && rect) openLinkChip(t, rect, locate, 'missing');
      return;
    }
    openConnection(res.note);
  }

  const linkCtx: LinkContext = {
    async openUrl(url: string) {
      try {
        await openUrl(url);
      } catch (err) {
        console.warn('[links] open url failed', err);
        error.set("Couldn't open the link.");
      }
    },
    openNote: openNoteLink,
    scrollToAnchor(id: string) {
      const passage = editorEl ? Array.from(editorEl.querySelectorAll('[id]')).find((el) => el.id === id) : undefined;
      passage?.scrollIntoView({ block: 'nearest' });
    },
    // `copyText` falls back past a refused `navigator.clipboard` and gives the
    // caret back; Copy is the only action an inert link or a missing note
    // has, so a copy that still fails must say so.
    copy(text: string) {
      void copyText(text).then((ok) => {
        if (!ok) error.set("Couldn't copy the link.");
      });
    },
    // The link prompt, prefilled from the anchor; OK replaces it in one undo step.
    // Never on a note that is not editable (the chip hides both; this is the
    // backstop).
    editLink(a: HTMLAnchorElement) {
      if (!editable) return;
      editLinkPrompt(a);
    },
    // Unwrap the anchor, keep its text: the user asked for this change.
    removeLink(a: HTMLAnchorElement) {
      if (!editable) return;
      const sel = window.getSelection();
      if (!sel || !editorEl?.contains(a)) return;
      const range = document.createRange();
      range.selectNodeContents(a);
      sel.removeAllRanges();
      sel.addRange(range);
      // A real webview fires its own `input` event during execCommand, which
      // already ran onInput; calling it again would count the edit twice.
      // Only where the command fired none (or does not exist) do we call it.
      const generation = editGeneration;
      document.execCommand('unlink');
      if (editGeneration === generation) onInput();
    },
  };

  // The hint goes on the editor element itself, never on the <a>: the body's
  // innerHTML is what gets saved, and a title attribute would sync to Apple.
  function onEditorMouseMove(e: MouseEvent) {
    const el = e.currentTarget as HTMLElement;
    const t = linkAt(e)?.target ?? null;
    const cursor = t ? 'pointer' : '';
    if (el.style.cursor !== cursor) el.style.cursor = cursor;
    const hint = t ? linkHint(t) : '';
    if (el.title !== hint) el.title = hint;
  }
  /** "<mod>+click to open <note title or URI>", or nothing when the modifier
   *  opens nothing (inert, a named app — those open from the chip only). */
  function linkHint(t: LinkTarget): string {
    if (t.kind === 'inert') return '';
    const scheme = schemeOf(t.uri);
    if (t.kind === 'os' && scheme && namedApp(scheme)) return '';
    let what = t.uri;
    if (t.kind === 'note' && t.key) {
      const p = linkPreviews.get(t.key);
      if (p === undefined) void requestLinkPreview(t.key); // the URI until it answers
      else if (p !== 'missing') what = p.title;
    }
    return `${modKey}+click to open ${what}`;
  }

  function onEditorScroll() {
    repositionLinkChip();
  }
  // Esc closes the chip and does nothing else — not the find bar, not a
  // dialog — but only when there was a chip to close. Capture phase on window
  // so it runs before any handler at the target.
  function onLinkChipKey(e: KeyboardEvent) {
    if (e.key !== 'Escape' || !linkChip) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    closeLinkChip();
  }
  // A press anywhere but the chip closes it (a press on another link closes
  // it here and its click opens the new one).
  function onLinkChipPointerDown(e: PointerEvent) {
    if (!linkChip) return;
    if (e.target instanceof Node && linkChipLayerEl?.contains(e.target)) return;
    closeLinkChip();
  }
  onMount(() => {
    window.addEventListener('keydown', onLinkChipKey, true);
    window.addEventListener('pointerdown', onLinkChipPointerDown, true);
  });
  onDestroy(() => {
    window.removeEventListener('keydown', onLinkChipKey, true);
    window.removeEventListener('pointerdown', onLinkChipPointerDown, true);
    if (linkScanPending) cancelAnimationFrame(linkScanFrame);
    linkScanPending = false;
  });

  // Updated-date display at top of note. The calendar (Gregorian / Buddhist
  // Era) comes from the Date display setting; see lib/format/dates.ts.

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

  // Options remembered while the app runs (agreed 2026-10-06): Aa makes the
  // search case-sensitive, .* reads the query as a regex — one line at a
  // time, ^ and $ per line, $1/$&/$$ in the replacement, zero-length matches
  // ignored (../editor/findMatches.ts).
  let findMatchCase = false;
  let findRegex = false;
  let findError = '';

  // A match can live either in the title input or in the body. The find/
  // replace UI treats them as a single ordered sequence (title matches first,
  // then body matches in document order) so that nextMatch can navigate
  // between them naturally.
  //
  // Apple Notes folds title-and-body into one editable surface (their
  // first body line IS the title), so find/replace there is body-only.
  // Jodd splits them — extending find/replace to both keeps the user's
  // mental model intact.
  type Match =
    | { kind: 'title'; lm: LineMatch }
    | { kind: 'body'; lm: LineMatch; range: Range; absStart: number; absEnd: number };

  // What Replace put in is not a match. Find ignores case by default, so a
  // replacement can still match the query: "line" → "LINE" re-replaced LINE1
  // on every press with the count stuck at 5 / 5, "note" → "notes" grew
  // "notess" (dev build, 2026-10-06). The inserted text is remembered as
  // [start, end) offsets — title offsets, and note-wide body offsets from
  // findBodyMatches — never as Ranges: a Gmail save re-renders a fresh editor
  // element and every Range would die with the old one. Forgotten when the
  // query, the replacement, an option, the note or its text changes.
  let replacedTitle: Array<[number, number]> = [];
  let replacedBody: Array<[number, number]> = [];
  let replacedNote = '';
  let findReplacing = false;

  function forgetReplaced() {
    replacedTitle = [];
    replacedBody = [];
  }

  const overlapsAny = (spans: Array<[number, number]>, start: number, end: number) =>
    spans.some(([a, b]) => start < b && end > a);

  // Records that [start, end) became `length` characters of replacement, and
  // moves every remembered span after it by the change in length.
  function recordReplaced(spans: Array<[number, number]>, start: number, end: number, length: number) {
    const delta = length - (end - start);
    const moved = spans.map(([a, b]): [number, number] => (a >= end ? [a + delta, b + delta] : [a, b]));
    if (length > 0) moved.push([start, start + length]);
    return moved;
  }

  function findAllMatches(query: string): Match[] {
    const compiled = compileFind(query, { matchCase: findMatchCase, regex: findRegex });
    findError = compiled.ok ? '' : compiled.error;
    if (!compiled.ok) return [];
    const key = $selectedNote ? noteKey($selectedNote) : '';
    if (key !== replacedNote) { forgetReplaced(); replacedNote = key; }
    const out: Match[] = [];

    // Title matches come first in the ordered sequence; the title is one line.
    if (title) {
      for (const lm of compiled.match(title)) {
        if (!overlapsAny(replacedTitle, lm.start, lm.end)) out.push({ kind: 'title', lm });
      }
    }

    // Then body matches in document order.
    if (editorEl) {
      for (const b of findBodyMatches(editorEl, compiled.match)) {
        if (!overlapsAny(replacedBody, b.absStart, b.absEnd)) {
          out.push({ kind: 'body', lm: b.match, range: b.range, absStart: b.absStart, absEnd: b.absEnd });
        }
      }
    }

    return out;
  }

  // While the bar is open the current match is PAINTED, never selected.
  // Inside a contenteditable the selection is the caret: putting it on a
  // match moved focus into the note, so the next keystroke meant for the
  // query overwrote the match ("hello" + "lo" → "lolo", 2026-10-06). The CSS
  // Custom Highlight API marks ranges without touching selection or focus.
  // A title match has nothing to paint — highlights cannot reach into an
  // <input>'s value — so it shows only in the count until Escape selects it.
  const FIND_HL = 'jodd-find';
  const FIND_HL_CURRENT = 'jodd-find-current';
  // Transparent. WebKit does not repaint a range that leaves EVERY highlight,
  // so a match that stopped matching kept its old colour until something
  // else repainted — linE1's "lin" stayed dark after typing "line" with Aa on
  // (dev build, and in Safari every time, 2026-10-06). Moving a range between
  // highlights does repaint it, so what was painted and no longer matches
  // spends one paint here. When it later leaves this one too, its stale
  // colour is transparent. CLAUDE.md gotcha #34.
  const FIND_HL_GONE = 'jodd-find-gone';
  let paintedRanges: Range[] = [];
  const canHighlight = () => typeof CSS !== 'undefined' && 'highlights' in CSS && typeof Highlight !== 'undefined';

  // Explicit priorities, above the link highlights (links/paint.ts,
  // LINK_HL_PRIORITY = 0): a match inside a link must still show as a match.
  const FIND_HL_PRIORITY = 1;
  const FIND_HL_CURRENT_PRIORITY = 2;
  function findHighlight(priority: number, ...ranges: Range[]): Highlight {
    const h = new Highlight(...ranges);
    h.priority = priority;
    return h;
  }
  function paintFindHighlights(matches: Match[]) {
    if (!canHighlight()) return;
    const body = matches.filter((m): m is Extract<Match, { kind: 'body' }> => m.kind === 'body').map((m) => m.range);
    const current = matches[currentMatchIdx];
    CSS.highlights.set(FIND_HL_GONE, findHighlight(FIND_HL_PRIORITY, ...paintedRanges));
    CSS.highlights.set(FIND_HL, findHighlight(FIND_HL_PRIORITY, ...body));
    CSS.highlights.set(FIND_HL_CURRENT, current?.kind === 'body' ? findHighlight(FIND_HL_CURRENT_PRIORITY, current.range) : findHighlight(FIND_HL_CURRENT_PRIORITY));
    paintedRanges = body;
  }

  function clearFindHighlights() {
    paintFindHighlights([]);
  }

  function scrollToCurrentMatch(matches: Match[]) {
    const m = matches[currentMatchIdx];
    if (m?.kind !== 'body') return;
    const rect = m.range.getBoundingClientRect();
    const editorRect = editorEl.getBoundingClientRect();
    if (rect.bottom > editorRect.bottom || rect.top < editorRect.top) {
      (m.range.startContainer.parentElement || editorEl).scrollIntoView({ block: 'center' });
    }
  }

  function selectMatch(idx: number, { scroll = true }: { scroll?: boolean } = {}) {
    const matches = findAllMatches(findQuery);
    matchCount = matches.length;
    if (!matchCount) { clearFindHighlights(); return; }
    currentMatchIdx = ((idx % matchCount) + matchCount) % matchCount;
    paintFindHighlights(matches);
    if (scroll) scrollToCurrentMatch(matches);
  }

  // Leaving the bar with Escape is the one moment the caret should go to the
  // match, so the user can edit right where they found it.
  function placeCaretOnCurrentMatch() {
    const matches = findAllMatches(findQuery);
    if (!matches.length) return;
    const m = matches[Math.min(currentMatchIdx, matches.length - 1)];
    if (m.kind === 'title') {
      const input = document.querySelector('.title-input') as HTMLTextAreaElement | null;
      input?.focus();
      input?.setSelectionRange(m.lm.start, m.lm.end);
    } else {
      editorEl.focus();
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(m.range);
    }
  }

  // The count and the paint follow every keystroke — a paint that waited
  // for the pause showed the previous query's matches beside the new count.
  // Only the scroll is debounced, so a multi-char query doesn't jump the
  // note on every keystroke.
  let findInputDebounce: ReturnType<typeof setTimeout> | undefined;
  function onFindInput() {
    forgetReplaced();
    selectMatch(0, { scroll: false });
    clearTimeout(findInputDebounce);
    findInputDebounce = setTimeout(() => selectMatch(0), 350);
  }

  function onReplaceInput() {
    forgetReplaced();
    selectMatch(currentMatchIdx);
  }

  function toggleFindOption(option: 'case' | 'regex') {
    if (option === 'case') findMatchCase = !findMatchCase;
    else findRegex = !findRegex;
    forgetReplaced();
    clearTimeout(findInputDebounce);
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
    const text = expandReplacement(replaceQuery, m.lm, findRegex);
    findReplacing = true;
    try {
      if (m.kind === 'title') {
        title = title.slice(0, m.lm.start) + text + title.slice(m.lm.end);
        replacedTitle = recordReplaced(replacedTitle, m.lm.start, m.lm.end, text.length);
      } else {
        replaceRanges(editorEl, [m.range], text);
        replacedBody = recordReplaced(replacedBody, m.absStart, m.absEnd, text.length);
      }
      onInput();
    } finally {
      findReplacing = false;
    }
    // What was replaced dropped out of the list, so `idx` is now the next one.
    setTimeout(() => selectMatch(idx), 0);
  }

  function replaceAll() {
    if (!findQuery) return;
    const matches = findAllMatches(findQuery);
    if (!matches.length) return;
    findReplacing = true;
    try {
      // Title first, building the new title left to right.
      const inTitle = matches.filter((m): m is Extract<Match, { kind: 'title' }> => m.kind === 'title');
      if (inTitle.length) {
        let out = '';
        let i = 0;
        for (const m of inTitle) {
          const text = expandReplacement(replaceQuery, m.lm, findRegex);
          const at = out.length + (m.lm.start - i);
          out += title.slice(i, m.lm.start) + text;
          replacedTitle = recordReplaced(replacedTitle, at, at + (m.lm.end - m.lm.start), text.length);
          i = m.lm.end;
        }
        title = out + title.slice(i);
      }

      // Then the body — as edit commands, so one Cmd+Z undoes every
      // replacement (../editor/bodyEdits.ts). Offsets are recorded in
      // document order, each shifted by the replacements before it.
      const inBody = matches.filter((m): m is Extract<Match, { kind: 'body' }> => m.kind === 'body');
      if (inBody.length && editorEl) {
        const texts = inBody.map((m) => expandReplacement(replaceQuery, m.lm, findRegex));
        replaceRanges(editorEl, inBody.map((m) => m.range), texts);
        let shift = 0;
        inBody.forEach((m, k) => {
          replacedBody = recordReplaced(replacedBody, m.absStart + shift, m.absEnd + shift, texts[k].length);
          shift += texts[k].length - (m.absEnd - m.absStart);
        });
      }
      onInput();
    } finally {
      findReplacing = false;
    }
    setTimeout(() => selectMatch(0), 0);
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
    findError = '';
    forgetReplaced();
    clearTimeout(findInputDebounce);
    clearFindHighlights();
  }

  function onFindKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      clearTimeout(findInputDebounce);
      placeCaretOnCurrentMatch();
      closeFind();
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      // User committed — cancel any pending debounced selectMatch(0) so it
      // doesn't snap them back to the first match after they pressed Enter.
      clearTimeout(findInputDebounce);
      if (e.shiftKey) prevMatch(); else nextMatch();
    }
  }

  // Cmd+F / Ctrl+F to open find bar — only when the editor is in scope.
  function onWindowKey(e: KeyboardEvent) {
    const cmd = isShortcutMod(e, $isMacos);
    if (cmd && e.key.toLowerCase() === 'f' && $selectedNote) {
      e.preventDefault();
      openFind();
    }
  }

  onMount(() => window.addEventListener('keydown', onWindowKey));
  onDestroy(() => {
    window.removeEventListener('keydown', onWindowKey);
    clearTimeout(findInputDebounce);
    if (canHighlight()) for (const name of [FIND_HL, FIND_HL_CURRENT, FIND_HL_GONE]) CSS.highlights.delete(name);
  });
</script>

{#if $selectedNote}
  <div class="editor-pane" class:format-bar-shown={formatBarShown}>
    <!-- Not on a phone (spec 2026-10-20 §5.6): there the note PaneHeader
         carries the save state, "Edited on another device" and ⋯, and the
         formatting buttons are FormatBar's, above the keyboard. -->
    {#if !phoneLayout}
    {#snippet fmtGlyph(id: FmtId)}
      {#if id === 'bold'}<b>B</b>
      {:else if id === 'italic'}<i>I</i>
      {:else if id === 'underline'}<u>U</u>
      {:else if id === 'strike'}<s>S</s>
      {:else if id === 'link'}<Icon name="link" size={14} />
      {:else if id === 'heading'}H
      {:else if id === 'quote'}<Icon name="quote" size={14} />
      {:else if id === 'code'}&lt;/&gt;
      {:else if id === 'bullets'}• —
      {:else if id === 'numbered'}1.
      {:else if id === 'task'}<Icon name="checkbox" size={14} />
      {/if}
    {/snippet}
    <!-- One button, two homes: the toolbar row and the More formatting menu.
         onmousedown preventDefault keeps focus + selection IN the editor so a
         click applies to the selected text instead of blurring. In the menu a
         pick also returns focus to the editor and closes the menu. -->
    {#snippet fmtButton(item: FmtItem, inMenu: boolean, hidden: boolean)}
      <button
        class="fmt-btn"
        class:fmt-menu-item={inMenu}
        class:fmt-overflowed={hidden}
        type="button"
        role={inMenu ? 'menuitem' : undefined}
        data-fmt={inMenu ? undefined : item.id}
        inert={hidden}
        title={item.title}
        aria-label={item.aria || inMenu ? item.title : undefined}
        onmousedown={(e) => e.preventDefault()}
        onclick={() => (inMenu ? runFmtFromMenu(item.run) : item.run())}
      >
        {#if inMenu}
          <span class="fmt-glyph" aria-hidden="true">{@render fmtGlyph(item.id)}</span>
          <span class="fmt-label">{item.title}</span>
        {:else}
          {@render fmtGlyph(item.id)}
        {/if}
      </button>
    {/snippet}
    <div class="editor-toolbar" use:watchToolbarOverflow>
      <div class="toolbar-actions">
        <!-- Task 6.4: the row never wraps. Buttons that do not fit stay in the
             DOM, hidden and inert (so they keep a measurable width), and move
             from the right into the "More formatting" popover; the SAME
             fmtButton snippet and handlers render them there. -->
        {#each fmtItems as item, i (item.id)}
          {@render fmtButton(item, false, i >= fmtItems.length - overflowN)}
        {/each}
        {#if showMoreFmt}
          <button
            bind:this={moreFmtBtn}
            class="fmt-btn more-fmt-btn"
            type="button"
            title="More formatting"
            aria-label="More formatting"
            aria-haspopup="menu"
            aria-expanded={moreFmtOpen}
            onmousedown={(e) => e.preventDefault()}
            onclick={() => (moreFmtOpen = !moreFmtOpen)}
          ><Icon name="chevron-down" size={14} /></button>
          {#if moreFmtBtn}
            <Popover anchor={moreFmtBtn} bind:open={moreFmtOpen} onclose={() => (moreFmtOpen = false)}>
              <div class="fmt-menu" role="menu" aria-label="More formatting" data-testid="more-formatting-menu">
                {#each fmtItems.slice(fmtItems.length - overflowN) as item (item.id)}
                  {@render fmtButton(item, true, false)}
                {/each}
              </div>
            </Popover>
          {/if}
        {/if}
      </div>
      <!-- Spec 2026-10-20 §6.1: the save state as short text, then ⋯. Each
           short string's former sentence is its title and accessible name
           (saveState.title, editorSaveState.ts). Graph, the shortcuts cheat
           sheet and Delete moved from here into ⋯ (NoteMoreMenu). -->
      <div class="toolbar-right">
        {#if saveState?.kind === 'saving'}
          <span class="save-status saving" role="status" title={saveState.title} aria-label={saveState.title}>{saveState.text}</span>
        {:else if saveState?.kind === 'unsaved'}
          <span class="save-status saving" role="status" title={saveState.title} aria-label={saveState.title}>{saveState.text}</span>
        {:else if saveState?.kind === 'draft'}
          <!-- tmp: UUID means the note exists only client-side; no save has
               reached Gmail yet. Distinguish from "Saved" so the user knows
               typing is required to persist. -->
          <span class="save-status draft" role="status" title={saveState.title} aria-label={saveState.title}>{saveState.text}</span>
        {:else if saveState?.kind === 'blocked'}
          <!-- The backend refused this push PERMANENTLY: retrying it unchanged
               can never work, so the worker has stopped. Local-first means we
               acknowledge a write before the remote confirms it — it does not
               mean claiming success for a write we know will never send.
               Before this, such a note read "Saved" here, kept a doomed push
               firing every 5s, and then dropped out of the note list entirely
               once the 30s "recently saved" grace window expired. -->
          <span class="save-status blocked" role="status" title={saveState.title} aria-label={saveState.title}>{saveState.text}</span>
          <button class="retry-sync-btn" onclick={retrySync} title={syncBlockNow?.reason}>
            Try again
          </button>
        {:else if saveState}
          <span class="save-status saved" role="status" title={saveState.title} aria-label={saveState.title}>{saveState.text}</span>
        {/if}
        {#if externalChangePending}
          <!-- Deliberately NOT part of the {#if} chain above. Those states
               describe this device's save; this describes the other device's,
               and the two are simultaneously true in exactly the case that
               matters — unsaved work here, a newer version there. Chaining it
               would hide whichever the author happened to put second. -->
          <span
            class="save-status external"
            role="status"
            title={EXTERNAL_CHANGE_TITLE}
            aria-label={EXTERNAL_CHANGE_TITLE}
          >
            {EXTERNAL_CHANGE_TEXT}
          </span>
          <button class="retry-sync-btn" onclick={reloadFromRemote} title={RELOAD_TITLE}>
            Reload
          </button>
        {/if}
        <button
          bind:this={moreBtn}
          class="fmt-btn more-btn"
          type="button"
          title="Note actions"
          aria-label="Note actions"
          aria-haspopup="menu"
          aria-expanded={moreShown}
          onclick={toggleMore}
        ><Icon name="more" size={14} /></button>
      </div>
    </div>
    {/if}
    <!-- Always mounted while it has an anchor (only `open` toggles), so its
         ConfirmDialog survives the menu closing. Tags… anchors to #N when the
         metadata line shows it, else to whichever ⋯ opened the menu. -->
    {#if moreAnchor}
      <NoteMoreMenu
        note={$selectedNote}
        anchor={moreAnchor}
        open={moreShown}
        onclose={() => (moreOpen = false)}
        onFind={openFind}
        onShowGraph={() => (showGraph = true)}
        onShortcuts={() => (showCheatsheet = true)}
        trustPending={!!trustChip}
        onMarkReviewed={markReviewedFromMenu}
        {reviewBlocked}
        onTags={() => { const a = metaTagsBtn ?? moreAnchor; if (a) openTags(a); }}
        showTags={!!meta && (meta.tagCount > 0 || meta.tagsEditable)}
      />
    {/if}

    {#if linkPromptOpen}
      <div class="link-prompt">
        {#if !linkPromptHasSelection}
          <input
            type="text"
            bind:value={linkPromptText}
            placeholder="Link text"
            class="link-prompt-input link-prompt-text"
            onkeydown={onLinkPromptKey}
          />
        {/if}
        <input
          type="url"
          bind:value={linkPromptUrl}
          bind:this={linkPromptUrlEl}
          placeholder="https://example.com"
          class="link-prompt-input link-prompt-url"
          onkeydown={onLinkPromptKey}
        />
        <button type="button" class="link-prompt-btn" onclick={applyLinkPrompt}>OK</button>
        <button type="button" class="link-prompt-btn link-prompt-cancel" onclick={closeLinkPrompt}>Cancel</button>
      </div>
    {/if}

    {#if showFind}
      <div class="find-bar">
        <div class="find-row">
          <svg width="12" height="12" viewBox="0 0 16 16" fill="none" style="color: var(--text-muted)">
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
          <button class="find-opt" class:on={findMatchCase} aria-label="Match case" aria-pressed={findMatchCase} title="Match case" onclick={() => toggleFindOption('case')}>Aa</button>
          <button class="find-opt mono" class:on={findRegex} aria-label="Regular expression" aria-pressed={findRegex} title="Regular expression (^ $ per line, $1 in Replace)" onclick={() => toggleFindOption('regex')}>.*</button>
          <span class="find-count" class:error={!!findError} title={findError}>
            {#if findError}invalid regex{:else if findQuery && matchCount > 0}{currentMatchIdx + 1} / {matchCount}{:else if findQuery}no match{/if}
          </span>
          <button class="find-btn" onclick={prevMatch} disabled={!matchCount} title="Previous (Shift+Enter)">‹</button>
          <button class="find-btn" onclick={nextMatch} disabled={!matchCount} title="Next (Enter)">›</button>
          <label class="find-toggle" title="Show replace">
            <input type="checkbox" bind:checked={showReplace} /> Replace
          </label>
          <button class="find-close" onclick={closeFind} aria-label="Close find" title="Close (Esc)"><Icon name="close" size={12} /></button>
        </div>
        {#if showReplace}
          <div class="find-row">
            <span style="width: 12px"></span>
            <input
              bind:value={replaceQuery}
              oninput={onReplaceInput}
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

    {#if meta}
      <!-- One line in place of the breadcrumb row, the centred date, the trust
           chip, the [[slug]] chip and the push/pin banners (spec 2026-10-20
           §6.1). The banners' sentences are in its status popover. -->
      <EditorMetaLine
        note={meta.note}
        accountLabel={meta.accountLabel}
        folderPath={meta.folderPath}
        onRevealFolder={revealFolder}
        status={meta.status}
        tagCount={meta.tagCount}
        tagsEditable={meta.tagsEditable}
        onOpenTags={openTags}
        bind:tagsButton={metaTagsBtn}
      />
      {#if tagsAnchor}
        <TagsPopover
          anchor={tagsAnchor}
          open={tagsShown}
          onclose={() => (tagsOpen = false)}
          tags={currentTags}
          editable={meta.tagsEditable}
          suggestions={tagSuggestions}
          onAdd={addTagFromPopover}
          onRemove={removeTagFromPopover}
          onabandon={(ops) => handOffTagEdits(tagEditReceiver, tagsFor, ops)}
          readOnlyHint={meta.note.uuid?.startsWith('tmp:') ? null : 'Add or remove tags in Apple Notes.'}
          readOnlyReason={meta.note.uuid?.startsWith('tmp:') ? null : tagsReadOnlyReason}
        />
      {/if}
    {/if}

    <!-- Read-only account (a backend whose `Writes.notes` is false):
         the backend refuses the save outright, so offering an editor that
         silently discards the user's typing would be worse than showing it
         locked. See `canWrite`. A visible notice, not a status in the line:
         it explains why typing does nothing. -->
    {#if !canWriteNotes}
      <div class="read-only-banner meta-notice" title="Read-only in Jodd for now — edit this note in Apple Notes and it will sync back.">
        Read-only in Jodd for now — edit this note in Apple Notes and it will sync back.
      </div>
    {/if}
    {#if contextNoticeText}
      <div class="context-notice meta-notice" class:two-lines={$isAndroid && $androidLayoutMode === 'phone'} role="status">
        <span title={contextNoticeText}>{contextNoticeText}</span>
        <button type="button" onclick={closeContextNote}>Close note</button>
      </div>
    {/if}
    {#if chipSuggestion && canRelocate}
      <div class="meta-notice folder-suggestion" data-testid="folder-suggestion" title={chipSuggestion.reason ?? ''}>
        <span class="folder-suggestion-text">Suggested folder: <span class="ctx-suggestion-path">{chipSuggestion.path}</span></span>
        <button type="button" class="ctx-suggestion-btn" onclick={acceptFolderSuggestion}>Move</button>
        <button type="button" class="ctx-suggestion-btn" onclick={dismissFolderSuggestion}>Keep here</button>
      </div>
    {/if}

    <!-- A one-row textarea that grows, so a long title wraps instead of
         being clipped — on every layout (phone: live pass 2026-10-21, "6
         Things You Should Never A"; tablet: Galaxy Tab S7 the same day, "…for
         basically fre"). It still holds one line: Enter goes to the body, and
         a pasted line break becomes a space (onTitleInput). -->
    <textarea
      class="title-input"
      rows="1"
      enterkeyhint="next"
      bind:this={titleEl}
      bind:value={title}
      onkeydown={onTitleKeydown}
      onbeforeinput={onTitleBeforeInput}
      oninput={onTitleInput}
      onfocus={onTitleFocus}
      onclick={onTitleClick}
      onfocusin={onEditingFocusIn}
      onfocusout={(e) => {
        reselectTitleOnClick = false;
        onEditingFocusOut(e);
      }}
      placeholder="Title"
      readonly={!editable}
    ></textarea>

    <!-- The invisible-title guard. A leading orphaned combining mark (e.g. a
         lone Thai sara-uee, U+0E37) is blank on Windows but real, stored, and
         synced to Apple, which draws it as a dotted circle plus the mark. Warn,
         show it the way iOS does, and offer a one-click removal rather than
         leaving the user with a title they cannot see. -->
    {#if titleHasOrphanMark}
      <div class="orphan-mark-warning">
        <span
          >This title starts with a hidden character that shows in Apple Notes as
          <strong>“{displayTitle(title)}”</strong>.</span
        >
        {#if editable}
          <button type="button" class="orphan-mark-remove" onclick={removeLeadingOrphanMarks}
            >Remove it</button
          >
        {/if}
      </div>
    {/if}

    {#key editorRenderKey}
      <div
        class="editor-body"
        contenteditable={editable}
        bind:this={editorEl}
        oninput={onInput}
        onclick={onEditorClick}
        onmousemove={onEditorMouseMove}
        onscroll={onEditorScroll}
        onkeydown={onEditorKeydown}
        onkeyup={onEditorKeyup}
        onpaste={onEditorPaste}
        role="textbox"
        aria-multiline="true"
        aria-label="Note body"
        aria-readonly={editable ? undefined : 'true'}
        tabindex={editable ? 0 : undefined}
        aria-activedescendant={activeCaretOption}
        onfocusin={onEditingFocusIn}
        onfocusout={(e) => { onEditingFocusOut(e); void onEditorFocusOut(); }}
        data-placeholder="Start writing… (type # > - or 1. for formatting · click ⓘ for shortcuts)"
      ></div>
    {/key}
    <!-- documented exemption: the ⓘ above lives inside a data-placeholder
         attribute string — an <Icon> component cannot be rendered inside
         an attribute value. -->
    {#if linkPickerOpen && linkResults.length}
      <div class="link-picker" style={linkStyle} role="listbox" aria-label="Notes to link" id={`${caretListUid}-link`}>
        {#each linkResults as c, i (c.uuid)}
          <button
            class="link-opt"
            class:active={i === linkIndex}
            role="option"
            id={`${caretListUid}-link-${i}`}
            aria-selected={i === linkIndex}
            tabindex="-1"
            type="button"
            onmousedown={(e) => { e.preventDefault(); pickLink(c); }}
          >
            <span class="link-opt-title" title={c.title || 'Untitled'}>{c.title || 'Untitled'}</span>
            <span class="link-opt-label">{c.label}</span>
          </button>
        {/each}
      </div>
    {/if}
    {#if linkChip}
      <!-- Fixed at the viewport origin so the chip's absolute position uses
           the same coordinates as linkPickerStyle (the caret lists' fixed
           ones). The layer passes clicks through; the chip takes them. -->
      <div class="link-chip-layer" bind:this={linkChipLayerEl}>
        <LinkChip
          target={linkChip.target}
          preview={linkChip.preview}
          style={linkChip.style}
          ctx={linkCtx}
          canEdit={editable}
          onclose={closeLinkChip}
        />
      </div>
    {/if}
    {#if hashtagItems.length && !titleFocused}
      <HashtagPicker
        items={hashtagItems}
        activeIndex={hashtagIndex}
        position={hashtagPosition}
        onPick={pickHashtag}
        touch={$isAndroid}
        id={`${caretListUid}-tag`}
      />
    {/if}

    <ConnectionsDisclosure
      sources={citations.length}
      links={connections.outgoing.length}
      backlinks={connections.backlinks.length}
      sourcesState={citationsFailed ? 'failed' : citationsLoading && !citations.length && !citationsSettled ? 'loading' : 'ok'}
      onRetrySources={refreshCitations}
    >
      <div class="connections">
        {#if connections.outgoing.length}
          <div class="conn-group">
            <span class="conn-label">→ Links to</span>
            {#each connections.outgoing as n (noteKey(n))}
              <button class="conn-link" type="button" title={n.title || 'Untitled'} onclick={() => openConnection(n)}
                >{n.title || 'Untitled'}</button>
            {/each}
          </div>
        {/if}
        {#if connections.backlinks.length}
          <div class="conn-group">
            <span class="conn-label">← Linked from</span>
            {#each connections.backlinks as n (noteKey(n))}
              <button class="conn-link" type="button" title={n.title || 'Untitled'} onclick={() => openConnection(n)}
                >{n.title || 'Untitled'}</button>
            {/each}
          </div>
        {/if}
        {#key $selectedNote ? noteKey($selectedNote) : ''}
          <SourceList urls={citations} loading={citationsLoading} failed={citationsFailed} onretry={refreshCitations} />
        {/key}
      </div>
    </ConnectionsDisclosure>

    <!-- The error bar moved to App.svelte's <ErrorBar />. `error` is a global
         store, and this editor is not always the mounted pane — App.svelte
         swaps it for TrashPreview in Recently Deleted — so a failure raised
         there rendered nowhere, then appeared under whatever note the user
         opened next. Do not re-add a copy here. -->

    {#if formatBarShown}
      <FormatBar commands={formatCommands} disabled={formatBarDisabled} />
    {/if}

    {#if textStyleOpen}
      <Modal bind:this={textStyleModal} size="sm" phone="sheet" title="Text style" onclose={onTextStyleClose}>
        <div class="text-styles">
          {#each TEXT_STYLES as s (s.tag)}
            <button
              type="button"
              class="text-style text-style-{s.tag}"
              aria-pressed={textStyleCurrent === s.tag}
              onclick={() => pickTextStyle(s.tag)}
            >{s.label}</button>
          {/each}
        </div>
      </Modal>
    {/if}

    {#if showCheatsheet}
      <Modal size="md" title="Keyboard shortcuts" onclose={() => (showCheatsheet = false)}>
        <table class="cheat-table">
          <tbody>
            <tr><th colspan="2">Formatting</th></tr>
            <tr><td>Undo</td><td><kbd>{modKey}</kbd>+<kbd>Z</kbd></td></tr>
            <tr><td>Redo</td><td><kbd>{modKey}</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd>{#if !$isMacos}{' or '}<kbd>Ctrl</kbd>+<kbd>Y</kbd>{/if}</td></tr>
            <tr><td>Bold</td><td><kbd>{modKey}</kbd>+<kbd>B</kbd></td></tr>
            <tr><td>Italic</td><td><kbd>{modKey}</kbd>+<kbd>I</kbd></td></tr>
            <tr><td>Underline</td><td><kbd>{modKey}</kbd>+<kbd>U</kbd></td></tr>
            <tr><td>Strikethrough</td><td><kbd>{modKey}</kbd>+<kbd>Shift</kbd>+<kbd>X</kbd></td></tr>
            <tr><td>Link</td><td><kbd>{modKey}</kbd>+<kbd>K</kbd></td></tr>
            <tr><td>Heading 1 / 2 / 3</td><td><kbd>{modKey}</kbd>+<kbd>{altKey}</kbd>+<kbd>1</kbd>/<kbd>2</kbd>/<kbd>3</kbd></td></tr>
            <tr><td>Bullet list</td><td><kbd>{modKey}</kbd>+<kbd>Shift</kbd>+<kbd>8</kbd></td></tr>
            <tr><td>Numbered list</td><td><kbd>{modKey}</kbd>+<kbd>Shift</kbd>+<kbd>7</kbd></td></tr>
            <tr><td>Checklist</td><td><kbd>{modKey}</kbd>+<kbd>Shift</kbd>+<kbd>9</kbd></td></tr>
            <tr><td>Blockquote</td><td><kbd>{modKey}</kbd>+<kbd>Shift</kbd>+<kbd>.</kbd></td></tr>
            <tr><td>Inline code</td><td><kbd>{modKey}</kbd>+<kbd>E</kbd></td></tr>
            <tr><td>Code block</td><td><kbd>{modKey}</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd></td></tr>
            <tr><th colspan="2">Typing shortcuts (live keystrokes only)</th></tr>
            <tr><td>Heading 1 / 2 / 3</td><td><kbd>#</kbd> / <kbd>##</kbd> / <kbd>###</kbd> + space</td></tr>
            <tr><td>Blockquote</td><td><kbd>&gt;</kbd> + space</td></tr>
            <tr><td>Bullet list</td><td><kbd>-</kbd> or <kbd>*</kbd> + space</td></tr>
            <tr><td>Numbered list</td><td><kbd>1.</kbd> + space</td></tr>
            <tr><th colspan="2">Editor</th></tr>
            <tr><td>Indent / outdent line</td><td><kbd>Tab</kbd> / <kbd>Shift</kbd>+<kbd>Tab</kbd></td></tr>
            <tr><td>Link to another note</td><td>type <kbd>[[</kbd></td></tr>
            <tr><td>Find / replace in note</td><td><kbd>{modKey}</kbd>+<kbd>F</kbd></td></tr>
            <tr><td>Open link at cursor</td><td><kbd>{modKey}</kbd>+<kbd>Enter</kbd></td></tr>
            <tr><td>Continue / exit checklist</td><td><kbd>Enter</kbd> on a checklist line</td></tr>
          </tbody>
        </table>
        <p class="cheat-note">
          Typing shortcuts fire only as you type — never on paste — so quoted email lines
          (<kbd>&gt;</kbd>) and pasted code (<kbd>#</kbd>) stay literal.
          Press <kbd>Backspace</kbd> right after a trigger to revert it.
          Inside a code block, paste always preserves the literal text.
        </p>
      </Modal>
    {/if}

    {#if showGraph && $selectedNote}
      <Modal bind:this={graphModal} size="lg" phone="page" title="Note graph" onclose={() => (showGraph = false)}>
        {#if graphNodes.length === 0}
          <div class="graph-empty">Nothing connects to this note yet. Add a folder, a #tag, or a [[link]] and it will show up here.</div>
        {:else}
          <svg viewBox="0 0 600 400" class="graph-svg">
            {#each graphNodes as gn (gn.label)}
              <line x1="300" y1="200" x2={gn.x} y2={gn.y} class="g-edge g-{gn.kind}" />
            {/each}
            {#each graphNodes as gn (gn.label)}
              <!-- Every node is actionable: notes open, folders navigate,
                   tags filter. role/tabindex/keydown so the graph is usable
                   without a mouse — an SVG <g> is not focusable by default. -->
              <g
                class="g-node g-{gn.kind} clickable"
                role="button"
                tabindex="0"
                aria-label={gn.kind === 'folder'
                  ? `Go to folder ${gn.label}`
                  : gn.kind === 'tag'
                    ? `Filter by ${gn.label}`
                    : `Open note ${gn.label}`}
                onclick={() => openGraphNode(gn)}
                onkeydown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openGraphNode(gn); }
                }}
              >
                <title>{gn.label}</title>
                <rect x={gn.x - 56} y={gn.y - 14} width="112" height="28" rx="9" />
                <text x={gn.x} y={gn.y}>{gtrunc(gn.label)}</text>
              </g>
            {/each}
            <g class="g-node g-center">
              <title>{$selectedNote.title || 'Untitled'}</title>
              <rect x="238" y="184" width="124" height="32" rx="9" />
              <text x="300" y="200">{gtrunc($selectedNote.title || 'Untitled')}</text>
            </g>
          </svg>
          <div class="graph-legend">
            <span class="lg lg-folder">folder</span>
            <span class="lg lg-tag">tag</span>
            <span class="lg lg-link">→ links to</span>
            <span class="lg lg-backlink">← linked from</span>
          </div>
        {/if}
      </Modal>
    {/if}

  </div>
{:else}
  <div class="empty-editor" bind:this={emptyEditorEl} tabindex="-1" role="region" aria-label="Editor context">
    {#if emptyKind === 'search'}
      <p class="empty-headline">No notes match "{$searchQuery.trim()}"</p>
      <button type="button" class="empty-action" onclick={() => searchQuery.set('')}>Clear search</button>
    {:else if emptyKind === 'view'}
      <!-- A Smart Folder or tag filter: a new note cannot be filed into it
           (NoteList's own empty state offers none there either). -->
      <p class="empty-headline">No notes in {$listTitle}</p>
    {:else if emptyKind === 'folder'}
      <p class="empty-headline">No notes in {$listTitle}</p>
      {#if $currentAccountCanWrite}
        {#if emptyBackendLabel}
          <p class="empty-line">Notes you add here sync to {emptyBackendLabel}.</p>
        {/if}
        <button
          type="button"
          class="empty-action primary"
          aria-keyshortcuts={$isMacos ? 'Meta+N' : 'Control+N'}
          onclick={newNoteFromEmpty}
        >New note <kbd>{modKey}+N</kbd></button>
      {:else}
        <p class="empty-line">{emptyBackendLabel ? `This ${emptyBackendLabel} account` : 'This account'} is read-only in Jodd.</p>
      {/if}
    {:else}
      <p>Select a note</p>
      <p class="empty-context">{emptyContextName}</p>
    {/if}
  </div>
{/if}

<style>
  .context-notice { color:var(--text); }
  .empty-context { overflow-wrap:anywhere; min-width:0; }
  .context-notice button { color:var(--text); background:var(--surface-editor); border:1px solid var(--border); border-radius:var(--radius-sm); padding:0 var(--space-2); font:inherit; cursor:pointer; }

  .editor-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    background: var(--surface-editor);
    height: 100dvh;
    overflow: hidden;
    /* Android draws edge to edge: the pane's last row (the Sources / Links
       disclosure) sat under the navigation bar and could not be tapped (live
       pass 2026-10-21). The same inset PhoneBar keeps; 0 on desktop. */
    box-sizing: border-box;
    padding-bottom: env(safe-area-inset-bottom);
  }
  /* The keyboard is up: it covers the navigation bar, and the format bar
     reserves its own room (.format-bar-shown .editor-body). */
  .editor-pane.format-bar-shown {
    padding-bottom: 0;
  }

  .editor-toolbar {
    display: flex;
    /* One row, always (Task 6.4): what does not fit moves into the More
       formatting menu instead of wrapping onto a second line. */
    flex-wrap: nowrap;
    gap: var(--space-2);
    align-items: center;
    justify-content: space-between;
    padding: 8px 20px;
    border-bottom: 1px solid var(--border-subtle);
    background: var(--surface-editor);
  }

  /* Save state, then ⋯, on the right (spec 2026-10-20 §6.1). */
  .toolbar-right {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    margin-left: auto;
    flex: 0 0 auto;
  }
  .toolbar-right .save-status { white-space: nowrap; }

  .save-status {
    font-size: var(--size-xs);
  }

  .save-status.saving { color: var(--text-muted); }
  .save-status.saved { color: var(--text-muted); }
  .save-status.draft { color: var(--accent-action); font-style: italic; }
  /* Louder than `draft`: a draft is a normal state the user is one keystroke
     from leaving, this one is their content sitting on one device. */
  .save-status.blocked { color: var(--danger); font-weight: 500; }
  /* --accent, not --danger: nothing has gone wrong yet. It is a heads-up that
     saving from here will fork the note, which the user may well still want to
     do. (tokens.css has --warn-wash but no --warn text colour; inventing a hex
     here would be one more colour that no theme owns.) */
  .save-status.external { color: var(--accent); font-weight: 500; margin-left: 8px; }

  .retry-sync-btn {
    margin-left: 6px;
    padding: 1px 6px;
    font-size: var(--size-xs);
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--text-muted);
    cursor: pointer;
  }
  .retry-sync-btn:hover { background: var(--surface-hover); color: var(--text); }

  .toolbar-actions {
    position: relative;
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-wrap: nowrap;
    align-items: center;
    gap: var(--space-1);
  }
  /* A button that moved into the menu: out of the flow but still laid out, so
     its width stays measurable. `inert` (on the element) takes it out of the
     tab order and the accessibility tree. */
  .toolbar-actions > .fmt-btn.fmt-overflowed {
    position: absolute;
    top: 0;
    left: 0;
    visibility: hidden;
    pointer-events: none;
  }
  .fmt-menu {
    display: flex;
    flex-direction: column;
    min-width: calc(var(--space-8) * 6);
    font-size: var(--size);
  }
  .fmt-btn.fmt-menu-item {
    justify-content: flex-start;
    gap: var(--space-3);
    width: 100%;
    padding: var(--space-1-5) var(--space-3);
    color: var(--text);
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .fmt-menu-item .fmt-glyph {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: var(--space-6);
  }

  .fmt-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    /* The overflow measurement (watchToolbarOverflow) reads each button's
       natural width: a button that shrank or wrapped its label would read
       narrower than it draws, and the row would keep one too many. */
    white-space: nowrap;
    flex-shrink: 0;
    padding: 4px 8px;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: var(--size);
    color: var(--text-muted);
    transition: background 0.15s;
  }

  .fmt-btn:hover {
    background: var(--surface-hover);
  }

  .find-bar {
    background: var(--surface-editor);
    border-bottom: 1px solid var(--border-subtle);
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
    background: var(--surface-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    padding: 4px 8px;
    font-size: var(--size-sm);
    line-height: var(--leading);
    font-family: inherit;
    outline: none;
  }
  .find-input:focus { border-color: var(--border-strong); }
  /* Painted by paintFindHighlights. :global because ::highlight() styles the
     element holding the text, and body markup comes from {@html} — unscoped. */
  :global(::highlight(jodd-find)) { background-color: var(--accent-wash-strong); }
  :global(::highlight(jodd-find-gone)) { background-color: transparent; }
  /* The current match is a FILL, not a lighter/darker wash: --accent-tint
     beside the 20% wash measured nearly identical in WebKit, and ‹ › looked
     like they did nothing. Same pair as the primary buttons (AskJoddModal). */
  :global(::highlight(jodd-find-current)) { background-color: var(--accent-action); color: var(--text-inverse); }
  /* Links drawn by src/lib/links/paint.ts. Same two-highlight dance as Find:
     a range that stops being a link spends one paint in -gone (gotcha #34). */
  :global(::highlight(jodd-link)) { color: var(--accent-action); text-decoration: underline; }
  :global(::highlight(jodd-link-gone)) { color: inherit; text-decoration: none; }
  .find-count {
    font-size: var(--size-xs);
    color: var(--text-muted);
    min-width: 56px;
    text-align: right;
  }
  .find-btn {
    width: 22px;
    height: 22px;
    background: var(--surface-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    color: var(--text-secondary);
    cursor: pointer;
    font-size: var(--size);
    line-height: var(--leading-tight);
  }
  .find-btn:hover:not(:disabled) { background: var(--surface-list); }
  .find-opt {
    height: 22px;
    padding: 0 6px;
    background: var(--surface-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    color: var(--text-secondary);
    cursor: pointer;
    font-size: var(--size-xs);
    line-height: var(--leading-tight);
  }
  .find-opt.mono { font-family: var(--font-mono); }
  .find-opt:hover { background: var(--surface-list); }
  .find-opt.on {
    background: var(--accent-tint);
    border-color: var(--accent-border);
    color: var(--accent-on-tint);
  }
  .find-count.error { color: var(--danger); }
  .find-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .find-btn-text {
    padding: 3px 10px;
    background: var(--surface-panel);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    font-size: var(--size-xs);
    color: var(--text);
    cursor: pointer;
    font-family: inherit;
  }
  .find-btn-text:hover:not(:disabled) { background: var(--surface-list); }
  .find-btn-text:disabled { opacity: 0.4; cursor: not-allowed; }
  .find-toggle {
    font-size: var(--size-xs);
    color: var(--text-secondary);
    display: flex;
    align-items: center;
    gap: 3px;
    cursor: pointer;
    user-select: none;
  }
  .find-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-muted);
    font-size: var(--size-sm);
    padding: 0 4px;
  }
  .find-close:hover { color: var(--text); }

  /* One-line notices under the metadata line (spec 2026-10-20 §6.1): the
     whole-account read-only notice, the context notice and the folder
     suggestion. Each stays on one line; its text truncates, its buttons do
     not. */
  .meta-notice {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    margin: var(--space-1) calc(var(--space-6) + var(--space-1)) 0;
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
    white-space: nowrap;
  }
  .meta-notice > span {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta-notice > button {
    flex: none;
  }
  /* A warning may take two lines (copy rule): this one is the instruction
     that explains why typing does nothing, so it is not cut to one. */
  .read-only-banner {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
    white-space: normal;
  }
  /* Phone: the context notice's sentence names two places; two lines. */
  .context-notice.two-lines > span {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    white-space: normal;
  }
  .ctx-suggestion-path {
    color: var(--text-secondary);
  }
  .ctx-suggestion-btn {
    border: none;
    background: var(--surface-hover);
    color: var(--text-secondary);
    font: inherit;
    line-height: var(--leading);
    padding: 0 var(--space-1-5);
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .ctx-suggestion-btn:hover {
    color: var(--accent-action);
  }

  .orphan-mark-warning {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 28px 8px;
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-panel);
    color: var(--text-muted);
    font-size: var(--size-xs);
  }

  .orphan-mark-warning strong {
    color: var(--text);
    font-weight: 600;
  }

  .orphan-mark-remove {
    flex: none;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 3px 10px;
    background: transparent;
    color: var(--text);
    font-size: var(--size-xs);
    cursor: pointer;
  }

  .orphan-mark-remove:hover {
    background: var(--surface-hover, var(--surface-panel));
  }

  .title-input {
    font-size: var(--size-xl);
    font-weight: 700;
    color: var(--text);
    line-height: var(--leading);
    border: none;
    outline: none;
    padding: 8px 28px 8px;
    background: transparent;
    width: 100%;
    font-family: inherit;
  }

  /* Wraps and grows with its text instead of clipping (every layout). */
  textarea.title-input {
    display: block;
    resize: none;
    overflow: hidden;
    field-sizing: content;
    min-height: calc(var(--size-xl) * var(--leading) + 16px);
    overflow-wrap: anywhere;
  }

  .title-input::placeholder {
    color: var(--text-muted);
  }

  .editor-body {
    flex: 1;
    padding: 8px 28px 28px;
    font-size: var(--size-md);
    line-height: var(--leading-body);
    color: var(--text);
    outline: none;
    overflow-y: auto;
    font-family: inherit;
  }

  /* The phone format bar (FormatBar.svelte) is fixed over the bottom of the
     pane, on the keyboard. The body is the scroller: scroll-padding keeps the
     caret's line above the bar when the browser scrolls it into view, and the
     extra bottom padding lets the last line scroll up past the bar at all.
     --kb-inset is the keyboard's overlap when the window does not resize
     (0 when it does, where the bar's bottom is the shrunk viewport's). */
  .format-bar-shown .editor-body {
    scroll-padding-bottom: calc(var(--touch-target) + var(--kb-inset, 0px));
    padding-bottom: calc(var(--space-6) + var(--space-1) + var(--touch-target) + var(--kb-inset, 0px));
  }

  /* The text-style sheet (Aa on the format bar): one 48px row per style,
     each previewing the style it applies. */
  .text-styles {
    display: flex;
    flex-direction: column;
  }
  .text-style {
    min-height: var(--touch-target);
    padding: 0 var(--space-4);
    text-align: left;
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .text-style[aria-pressed='true'] {
    background: var(--surface-active);
  }
  .text-style-h1 { font-size: var(--size-xl); font-weight: 700; }
  .text-style-h2 { font-size: var(--size-lg); font-weight: 700; }
  .text-style-h3 { font-size: var(--size-md); font-weight: 600; }
  .text-style-div { font-size: var(--size-md); }

  /* Inline styles inside body_html win over anything we set on .editor-body,
     because they are element-level. 11 of 188 notes in a real vault carry
     font-family / background-color baked in by Apple Notes or by pasted
     content.

     Neutralised here at RENDER time, never by rewriting body_html -- that
     content round-trips to Apple Notes and mutating it is a correctness
     change dressed up as a styling one.

     Deliberately surgical. font-family and background-color are properties
     the app owns: a note has no legitimate reason to pick its own UI face,
     and a hardcoded white background is a dark-mode landmine. `color` is NOT
     overridden -- a span the user deliberately made red is content, and
     flattening it would be destroying information the user put there. */
  .editor-body :global([style*='font-family']) {
    font-family: var(--font-sans) !important;
  }
  /* Only NEAR-WHITE and faint-neutral backgrounds are neutralised, not every
     background-color. The blanket rule this replaces also erased a highlight
     the user deliberately applied in Apple Notes, which is content by exactly
     the argument used above to preserve `color`.

     Enumerated rather than computed, for two reasons. CSS cannot do luminance
     maths. And doing it in JS is not an option here: .editor-body IS the
     contenteditable whose innerHTML gets saved, so mutating its DOM would
     write the change back into body_html and out to Apple Notes — the one
     thing this whole override exists to avoid.

     Measured against the real vault (2026-07-29): across all 188 notes there
     are only four distinct background values — pure white (15 uses), off-white
     (2), and neutral black at 4% and 6% alpha (6 between them). All white or
     faint grey; zero highlights. The list below covers every one, plus the
     common spellings of white. A yellow, green or pink highlight matches
     nothing here and survives. Add a value if a new near-white turns up. */
  .editor-body :global([style*='background-color: rgb(255, 255, 255)' i]),
  .editor-body :global([style*='background-color:rgb(255,255,255)' i]),
  .editor-body :global([style*='background-color: rgb(253, 253, 252)' i]),
  .editor-body :global([style*='background-color: #fff' i]),
  .editor-body :global([style*='background-color:#fff' i]),
  .editor-body :global([style*='background-color: white' i]),
  .editor-body :global([style*='background-color: rgba(10, 10, 10, 0.0' i]),
  .editor-body :global([style*='background-color: rgba(0, 0, 0, 0.0' i]) {
    background-color: transparent !important;
  }

  /* Blockquote: visually distinct from regular text — left bar + indent +
     muted color so a `> ` line reads as a quote, not a paragraph. */
  .editor-body :global(blockquote) {
    margin: 6px 0;
    padding: 2px 0 2px 14px;
    border-left: 3px solid var(--accent);
    color: var(--text-secondary);
    font-style: italic;
  }
  /* Outline lists: indent + a distinct bullet per nesting level so Tab-nested
     items read as a hierarchy. :global — list markup is injected via innerHTML. */
  .editor-body :global(ul),
  .editor-body :global(ol) {
    padding-left: 26px;
    margin: 0;
  }
  /* Links in Jodd's text accent, not the webview's default blue: a plain
     click opens the link chip, Cmd/Ctrl+click opens them (onLinkClick). The
     underline stays so a link never rests on colour alone. */
  .editor-body :global(a) { color: var(--accent-action); }
  .editor-body :global(ul) { list-style: disc; }
  .editor-body :global(ul ul) { list-style: circle; }
  .editor-body :global(ul ul ul) { list-style: square; }

  .editor-body:empty::before {
    content: attr(data-placeholder);
    color: var(--text-muted);
    pointer-events: none;
  }

  .empty-headline {
    margin: 0;
    color: var(--text);
    font-size: var(--size-lg);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .empty-line { margin: 0; overflow-wrap: anywhere; }
  .empty-action {
    margin-top: var(--space-2);
    padding: var(--space-1-5) var(--space-3);
    min-height: var(--hit-target-desktop);
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-panel);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .empty-action:hover { background: var(--surface-hover); }
  .empty-action.primary {
    background: var(--accent-action);
    border-color: var(--accent-action);
    color: var(--text-inverse);
  }
  .empty-action.primary:hover { background: var(--accent-hover); }
  .empty-action kbd {
    margin-left: var(--space-2);
    font: inherit;
    font-size: var(--size-sm);
    opacity: 0.85;
  }
  .empty-editor {
    flex: 1;
    flex-direction: column;
    gap: 8px;
    padding: 16px;
    text-align: center;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
    font-size: var(--size-md);
    background: var(--surface-editor);
  }

  .link-chip-layer {
    position: fixed;
    inset: 0;
    z-index: 1000;
    pointer-events: none;
  }

  .link-picker {
    position: fixed;
    z-index: 1000;
    min-width: 220px;
    max-width: 340px;
    max-height: 240px;
    overflow-y: auto;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-menu);
    padding: 4px;
  }
  .link-opt {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    border: none;
    background: none;
    text-align: left;
    padding: 6px 9px;
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
  }
  .link-opt:hover,
  .link-opt.active {
    background: var(--accent-tint);
  }
  .link-opt-title {
    font-size: var(--size);
    color: var(--text);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
  }
  .link-opt-label {
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
    white-space: nowrap;
  }

  .graph-empty {
    color: var(--text-muted);
    font-size: var(--size);
    text-align: center;
    padding: 40px 0;
  }
  .link-prompt {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 8px 12px;
    background: var(--surface-list);
    border-bottom: 1px solid var(--border-subtle);
  }
  .link-prompt-input {
    flex: 1;
    padding: 5px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: var(--size);
    line-height: var(--leading);
    background: var(--surface-panel);
    color: var(--text);
    font-family: inherit;
  }
  .link-prompt-text {
    flex: 0 0 30%;
  }
  .link-prompt-input:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: var(--ring-focus);
  }
  .link-prompt-btn {
    padding: 5px 12px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface-panel);
    color: var(--text);
    font-size: var(--size-sm);
    cursor: pointer;
  }
  .link-prompt-btn:hover {
    background: var(--accent-tint);
  }
  .link-prompt-cancel {
    color: var(--text-muted);
  }
  .cheat-table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--size);
    color: var(--text);
  }
  .cheat-table th {
    text-align: left;
    padding: 12px 0 4px;
    font-size: var(--size-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    border-bottom: 1px solid var(--border-subtle);
  }
  .cheat-table td {
    padding: 4px 0;
    vertical-align: top;
  }
  .cheat-table td:last-child {
    text-align: right;
    white-space: nowrap;
    color: var(--text-secondary);
  }
  .cheat-table kbd {
    display: inline-block;
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: 4px;
    background: var(--surface-list);
    font-family: var(--font-mono);
    font-size: var(--size-xs);
    color: var(--text);
  }
  .cheat-note {
    margin: 14px 0 0;
    padding: 10px 12px;
    background: var(--surface-list);
    border-left: 3px solid var(--accent);
    border-radius: 4px;
    font-size: var(--size-sm);
    line-height: 1.5;
    color: var(--text-muted);
  }
  .cheat-note kbd {
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: var(--surface-panel);
    font-family: var(--font-mono);
    font-size: var(--size-xs);
  }
  .graph-svg {
    width: 100%;
    height: auto;
    display: block;
  }
  .g-edge {
    stroke-width: 1.5;
  }
  .g-edge.g-folder { stroke: var(--graph-folder); }
  .g-edge.g-tag { stroke: var(--graph-tag); }
  .g-edge.g-link { stroke: var(--graph-link); }
  .g-edge.g-backlink { stroke: var(--graph-backlink); }
  .g-node rect { stroke-width: 1.2; }
  .g-node text {
    font-size: var(--size-xs);
    text-anchor: middle;
    dominant-baseline: middle;
    fill: var(--text);
  }
  .g-node.clickable { cursor: pointer; }
  .g-node.clickable:hover rect { filter: brightness(0.95); }
  /* brightness(0.95) darkens — right on the light fills, invisible on the dark
     theme's low-luminance ones, so dark lightens instead. Mirrors the token
     architecture exactly: the media query follows the OS, the attribute
     overrides it in both directions. `:not([data-theme='light'])` alone would
     be wrong — with no attribute set (System) it also matches a light OS. */
  @media (prefers-color-scheme: dark) {
    :global(:root:not([data-theme='light'])) .g-node.clickable:hover rect {
      filter: brightness(1.25);
    }
  }
  :global(:root[data-theme='dark']) .g-node.clickable:hover rect {
    filter: brightness(1.25);
  }
  .g-node.clickable:focus-visible { outline: none; }
  .g-node.clickable:focus-visible rect { stroke-width: 3; filter: brightness(1.1); }
  .g-center rect { fill: var(--accent-on-tint); stroke: var(--accent-on-tint); }
  .g-center text { fill: var(--surface-panel); font-weight: 600; }
  .g-folder rect { fill: var(--graph-folder-fill); stroke: var(--graph-folder); }
  .g-tag rect { fill: var(--graph-tag-fill); stroke: var(--graph-tag); }
  .g-link rect { fill: var(--graph-link-fill); stroke: var(--graph-link); }
  .g-backlink rect { fill: var(--graph-backlink-fill); stroke: var(--graph-backlink); }
  .graph-legend {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
    margin-top: 8px;
    font-size: var(--size-xs);
    color: var(--text-muted);
  }
  /* documented exemption: ● here is a CSS content: bullet, not markup —
     an <Icon> component cannot be rendered inside a content: value. */
  .lg::before { content: '●'; margin-right: 4px; }
  .lg-folder::before { color: var(--graph-folder); }
  .lg-tag::before { color: var(--graph-tag); }
  .lg-link::before { color: var(--graph-link); }
  .lg-backlink::before { color: var(--graph-backlink); }

  /* The disclosure line owns the border and the horizontal padding. */
  .connections {
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex-shrink: 0;
  }
  .conn-group {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .conn-label {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--size-xs);
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .conn-link {
    border: none;
    background: var(--surface-hover);
    color: var(--text-secondary);
    font-size: var(--size-sm);
    line-height: var(--leading);
    font-family: inherit;
    padding: 2px 9px;
    border-radius: 11px;
    cursor: pointer;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-decoration: none;
    display: inline-block;
  }
  .conn-link:hover {
    background: var(--accent-wash);
    color: var(--text);
  }

</style>
