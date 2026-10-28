<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import { appendHtml } from '../noteHtml';
  import { listen } from '@tauri-apps/api/event';
  import ActionItemsReview from './ActionItemsReview.svelte';
  import type { ActionItemsPreview } from '../meetingActions';
  import { automaticEnrichmentEnabled } from "../aiLimits";
  import AiReceipts from "./AiReceipts.svelte";
  import { invoke, Channel } from '@tauri-apps/api/core';
  import { get } from 'svelte/store';
  import {
    currentAccount,
    capabilitiesByAccount,
    notes,
    selectedNote,
    selectedFolder,
    navigateToFolder,
    accounts,
    accountDisplayById,
    canWrite,
    setAccountNoteTags,
    indexUpsertOnSave,
  } from '../stores/notes';
  import type { Note, IngestAnalysis, IngestProgress, ExtractedNote, ModalWorkflow } from '../types';
  import LinkSuggestionsModal from './LinkSuggestionsModal.svelte';
  import Modal from './Modal.svelte';
  import type { Pane } from '../stores/phoneNav';
  import { viewName } from '../viewName';
  import { keywordsHintShown, KEYWORDS_HINT, tagModelOf } from '../editor/bodyTags';
  import { extractPrefill, drainCaptures, pendingCaptures } from '../capture';
  import { settingsRoute } from '../stores/ui';
  import { requestFolderSuggestion, recordFolderSuggestion } from '../folderSuggestion';
  import { refreshUnreviewed } from '../unreviewed';
  import { openCreatedNote } from '../openCreatedNote';
  import { hiddenFromAi, aiHiddenFor, aiSourceFor, AI_HIDDEN_HINT } from '../stores/aiScope';
  import { isConsentRefusal, allowAiForAccount, allowAiLabel } from '../aiConsent';
  import { ANALYZE_DEBOUNCE_MS, MAX_URLS_PER_INGEST, WORKFLOW_OPTIONS, offersOwnTranscription, initialSelection, progressLine, sourceKindLabel, toggleUrl, backendWorkflow, ingestModeFor } from '../ingestSources';

  // Bindable so the parent can both open and observe close.
  let { open = $bindable(false) }: { open: boolean } = $props();

  let sourceText = $state('');
  // The source and title a capture handed over (see `dirty`); empty otherwise.
  let pristine = $state({ text: '', title: '' });
  let sourceIncomplete = $state(false);
  let titleOverride = $state('');
  let busy = $state(false);
  let errorMsg = $state('');
  // The account a consent refusal in `errorMsg` belongs to. Set together
  // with errorMsg in the same turn (gotcha #28), never read from
  // $currentAccount at click time (pre-flight W3).
  let consentAccount = $state('');
  // request_id for the in-flight extract — generated per extraction so the
  // Cancel button can pass it to cancel_extraction. Empty when no extract
  // is running.
  let requestId = $state('');
  let receiptRequestId = $state('');

  let actionPreview = $state<ActionItemsPreview | null>(null);
  let previewAccount = '';
  let previewGeneration = 0;
  let disposed = false;
  let pickerAccount = get(currentAccount);
  function discardPreview() {
    previewGeneration++;
    if (actionPreview) void invoke('discard_action_items', { aiResultId: actionPreview.ai_result_id }).catch(() => {});
    actionPreview = null;
  }
  $effect(() => {
    const acct = $currentAccount;
    if (acct !== pickerAccount) {
      pickerAccount = acct;
      untrack(() => {
        clearTimeout(targetSearchTimer); clearTimeout(sourceNoteSearchTimer);
        targetSearchSeq++; sourceNoteSearchSeq++;
        targetNote = null; targetQuery = ''; targetResults = [];
        sourceNote = null; sourceNoteQuery = ''; sourceNoteResults = [];
        if (sourceMode === 'existing') sourceText = '';
        // A refusal belongs to the account it was for.
        errorMsg = '';
        consentAccount = '';
      });
    }
    if (previewAccount && acct !== previewAccount) {
      untrack(() => { discardPreview(); void cancelInFlight(); });
    }
  });
  $effect(() => { if (!open) untrack(() => { discardPreview(); void cancelInFlight(); }); });

  // Share to Jodd ("Summarize with AI" in CaptureSheet): the capture this
  // OPENING of the modal came from. Reset on every fresh open — never on
  // close, because every success path closes the modal before it reaches
  // showCreatedNote — so a later, unrelated extract can't retire a capture
  // the person put off.
  let fromCapture: string | null = null;
  let wasOpen = false;
  $effect(() => {
    const isOpen = open;
    const prefill = $extractPrefill;
    untrack(() => {
      if (isOpen && !wasOpen) fromCapture = null;
      wasOpen = isOpen;
      if (isOpen && prefill) {
        extractPrefill.set(null);
        resetForm();
        fromCapture = prefill.captureId;
        sourceText = prefill.text;
        titleOverride = prefill.title;
        // What the sheet handed over is not the person's unsaved input: the
        // capture stays queued, so closing loses nothing (see `dirty`).
        pristine = { text: prefill.text, title: prefill.title };
        if (prefill.workflow) workflow = prefill.workflow;
      }
    });
  });
  function retireCapture() {
    const id = fromCapture;
    fromCapture = null;
    if (!id) return;
    // Optimistic and synchronous: every success path has already closed the
    // modal, which un-hides the capture sheet — without this the sheet would
    // flash the capture whose note now exists, with Save still live.
    pendingCaptures.update((list) => list.filter((c) => c.id !== id));
    void invoke('complete_capture', { captureId: id }).then(() => drainCaptures()).catch(() => {});
  }
  onMount(() => {
    let stop: (() => void) | undefined;
    void listen('ai-policy-changed', () => {
      discardPreview();
      void cancelInFlight();
    }).then(fn => { if (disposed) fn(); else stop = fn; }).catch(() => {});
    return () => stop?.();
  });
  onDestroy(() => { disposed = true; discardPreview(); void cancelInFlight(); });

  // A refusal is for the account the call ran for. The modal stays open across
  // sidebar switches, so a late refusal may arrive after the user moved on:
  // the text is still shown, but Allow is offered only while that account is
  // still current, or it would grant an account the user did not refuse.
  function offerConsentFor(acct: string | null): string {
    return acct && get(currentAccount) === acct ? acct : '';
  }

  async function applyActions() {
    if (!actionPreview || busy) return;
    const acct = previewAccount;
    const id = actionPreview.ai_result_id;
    busy = true;
    errorMsg = '';
    try {
      const created = await invoke<ExtractedNote>('apply_action_items', { accountId: acct, aiResultId: id });
      void refreshUnreviewed([acct]); // R15: an AI write makes a note unreviewed with no remote-changed behind it.
      actionPreview = null;
      if (!disposed && get(currentAccount) === acct) {
        closeIntoResult(created.label);
        resetForm();
        await showCreatedNote(acct, created, receiptRequestId);
      }
    } catch (e) {
      discardPreview();
      errorMsg = String(e);
      consentAccount = offerConsentFor(acct);
    } finally { busy = false; }
  }

  // roadmap #2 / spec 2026-09-27: which LLM workflow to run. 'extract' is
  // the default and the only historical behavior (extract_note /
  // append_extract_note, unchanged); any other value branches extract() to
  // run_llm_workflow / append_llm_workflow_note instead. Destination, source
  // mode, the duplicate-citation check and every post-save step behave
  // identically regardless of this value.
  let workflow = $state<ModalWorkflow>('extract');
  // Transcript's Clean option. Only a link can be ingested raw; pasted text
  // is always cleaned (spec D5) — see `effectiveClean`.
  let cleanTranscript = $state(true);

  // One name for this dialog everywhere (spec 2026-10-28 §3): its title is
  // fixed, and the selected option (Key points, Summarize, …) is what the
  // selector shows. The option list lives in ingestSources.ts: Share to
  // Jodd's capture sheet offers the same modes and must not drift from it.
  const DIALOG_TITLE = 'Make a note with AI';
  const WORKFLOW_HINTS: Record<ModalWorkflow, string> = {
    extract: 'Jodd will distill it into structured key points',
    summarize: 'Jodd will summarize it',
    action_items: 'Jodd will prepare decisions, actions, owners, dates and source evidence for review',
    expand_bullets: 'Jodd will expand its bullet points into full prose',
    transcript: 'Jodd will turn it into readable text, keeping all of it',
  };

  function setWorkflow(next: ModalWorkflow) {
    discardPreview();
    workflow = next;
  }

  // Populated by check_duplicate_citations when the pasted source's URL(s)
  // already exist as a citation elsewhere in this account. Non-empty → the
  // modal shows a soft warning with a "Continue anyway" resubmit instead of
  // calling the LLM. Never a hard block (see design spec decision 2).
  let duplicateWarnings = $state<
    { url: string; existing_note_uuid: string; existing_note_title: string }[]
  >([]);

  // Populated after a successful extract/append by suggest_wiki_links.
  // Non-empty proposedAppends opens LinkSuggestionsModal for confirmation —
  // auto_links have ALREADY been inserted into the note by this point
  // (design spec decision 5: automatic, no confirmation needed for the
  // note's own outgoing links).
  let linkSuggestionsOpen = $state(false);
  let proposedAppends = $state<{ uuid: string; title: string; addition_text: string }[]>([]);

  // Runs after extract_note/append_extract_note succeeds (design
  // spec decision 3: automatic, both paths). Failure here is a SOFT
  // failure — logs and returns silently, never surfaces an error to the
  // user or undoes the primary save.
  async function runAutoLinkSuggestions(
    acct: string,
    savedUuid: string,
    savedTitle: string,
    savedBody: string,
    parentRequestId: string,
  ) {
    try {
      const res = await invoke<{
        ai_result_id: string;
        auto_links: { uuid: string; title: string; slug: string }[];
        proposed_appends: { uuid: string; title: string; addition_text: string }[];
      }>('suggest_wiki_links', {
        accountId: acct,
        text: savedBody,
        excludeUuid: savedUuid,
        newNoteTitle: savedTitle,
        newNoteUuid: savedUuid,
        requestId: newRequestId(),
        parentRequestId,
      });

      if (res.auto_links.length > 0) {
        const linksLine = `<p>Related: ${res.auto_links
          .map((l) => `[[${l.slug}]]`)
          .join(', ')}</p>`;
        // $notes is a view of whichever folder last painted, not the truth: a
        // folder settle that runs before the new note reaches the remote
        // drops it (measured 2026-09-16). Fall back to SQLite, and write
        // nothing if the row is in neither — `label: ''` would file the note
        // under the root `Notes`, moving it out of Inbox.
        const current =
          get(notes).find((n: Note) => n.account_id === acct && n.uuid === savedUuid) ??
          (await invoke<Note[]>('list_cached_notes', { accountId: acct })).find(
            (n: Note) => n.account_id === acct && n.uuid === savedUuid,
          );
        if (!current) {
          console.warn('auto-link: saved note not found locally — skipping the Related line', savedUuid);
        } else {
          // save_note's real signature (lib.rs:1168-1180) requires
          // existingGmailId/existingUuid/existingXMailCreatedDate — passing
          // `uuid` (not a real param) and omitting the rest would silently
          // create a SECOND note instead of editing this one, since a missing
          // existingUuid makes the backend treat it as brand-new (lib.rs:1186-1191).
          await invoke('save_note', {
            aiResultId: res.ai_result_id ?? '',
            accountId: acct,
            title: savedTitle,
            bodyHtml: appendHtml(current.body_html ?? savedBody, linksLine),
            existingGmailId: current.id ?? null,
            existingUuid: savedUuid,
            existingXMailCreatedDate: current.x_mail_created_date ?? null,
            label: current.label,
          });
          void refreshUnreviewed([acct]); // R15: an AI write makes a note unreviewed with no remote-changed behind it.
        }
      }

      if (res.proposed_appends.length > 0) {
        proposedAppends = res.proposed_appends;
        linkSuggestionsOpen = true;
      }
    } catch (e) {
      console.warn('suggest_wiki_links failed — skipping auto-link suggestions', e);
    }
  }

  // Automatic caller of suggest_note_folder (extract filing). SOFT failure,
  // like auto-link: logged, never shown — the note is already saved and a
  // missing proposal costs nothing. Every non-suggestion outcome is silent.
  async function runFolderSuggestion(acct: string, uuid: string, parentRequestId: string) {
    try {
      recordFolderSuggestion(acct, uuid, await requestFolderSuggestion(acct, uuid, parentRequestId));
    } catch (e) {
      console.warn('suggest_note_folder failed — no folder suggestion', e);
    }
  }

  // Both post-Extract LLM calls, strictly in order: nothing records whether
  // two agent-CLI subprocesses may run at once (gotcha #7). Counts as part of
  // the Extract the user triggered (spec Decision 8) — roadmap #0b's
  // per-account gate on untriggered calls must cover both.
  async function runPostExtractSuggestions(acct: string, uuid: string, title: string, body: string, parentRequestId: string) {
    if (!(await automaticEnrichmentEnabled())) return;
    await runFolderSuggestion(acct, uuid, parentRequestId);
    if (!(await automaticEnrichmentEnabled())) return;
    await runAutoLinkSuggestions(acct, uuid, title, body, parentRequestId);
  }

  // Destination: 'new' creates a fresh note in Notes/Inbox (or the root);
  // 'existing' appends into a note the user picks below.
  type Destination = 'new' | 'existing';
  let destination = $state<Destination>('new');

  let targetQuery = $state('');
  let targetResults = $state<Note[]>([]);
  let targetNote = $state<Note | null>(null);
  let targetSearchTimer: ReturnType<typeof setTimeout> | undefined;
  let targetSearchSeq = 0;

  // Source: 'paste' is today's behavior (paste raw text). 'existing' feeds
  // an already-saved note's body through the same pipeline as pasted text —
  // the source note itself is never modified (design spec #2a).
  type SourceMode = 'paste' | 'existing';
  let sourceMode = $state<SourceMode>('paste');

  let sourceNoteQuery = $state('');
  let sourceNoteResults = $state<Note[]>([]);
  let sourceNote = $state<Note | null>(null);
  const sourceHidden = $derived(
    sourceMode === 'existing' && !!sourceNote &&
      aiHiddenFor($hiddenFromAi, sourceNote.account_id || get(currentAccount), sourceNote.label),
  );
  let sourceNoteSearchTimer: ReturnType<typeof setTimeout> | undefined;
  let sourceNoteSearchSeq = 0;

  // ── URL ingest (spec 2026-09-15-url-ingest-design.md) ──────────────────
  let analysis = $state<IngestAnalysis | null>(null);
  let selectedUrls = $state<string[]>([]);
  let progressLog = $state<string[]>([]);
  let analyzeTimer: ReturnType<typeof setTimeout> | undefined;
  let analyzeSeq = 0;

  const supportedCount = $derived(analysis ? analysis.sources.filter((s) => s.supported).length : 0);
  // Ingest always creates a new note: in append mode the section is shown disabled.
  const ingesting = $derived(destination === 'new' && selectedUrls.length > 0);
  // Gotcha #28: derived, not patched in an $effect — the selection, the
  // title, the active button and what gets submitted change in one turn.
  // Removing the links restores the user's Action items choice.
  const effectiveWorkflow = $derived<ModalWorkflow>(ingesting && workflow === 'action_items' ? 'extract' : workflow);
  const effectiveClean = $derived(!ingesting || cleanTranscript);
  // Spec D5 / §5.5: say where audio would go BEFORE the run. A link whose audio
  // may be transcribed is a TikTok / Instagram / Facebook / X one; derived from
  // the selection so it appears and goes in the same turn as the check box.
  const mayTranscribe = $derived(
    ingesting && !!analysis && analysis.sources.some((s) => selectedUrls.includes(s.url) && s.may_transcribe),
  );
  const sttHost = $derived(analysis?.stt_host ?? null);
  const sttInsecure = $derived(analysis?.stt_insecure ?? false);
  // "Transcribe the audio myself" (D2): ticked AND still offered. Derived so a
  // box that has gone away cannot leave a stale `true` to be submitted.
  const offerOwnTranscription = $derived(ingesting && offersOwnTranscription(analysis, selectedUrls));
  let transcribeMyself = $state(false);
  const transcribeAudio = $derived(offerOwnTranscription && transcribeMyself);
  // Spec 2026-10-11: on iCloud the LLM's suggested tags are saved as a plain
  // Keywords line (llm::markdown::tags_line). Reads $currentAccount because
  // that is what extract() and ingest() write to; tagModelOf so it is right
  // before capabilities load; derived in the same turn as the workflow it
  // depends on (gotcha #28).
  const keywordsHint = $derived(
    keywordsHintShown(
      tagModelOf($currentAccount ? $capabilitiesByAccount[$currentAccount] : undefined, $currentAccount),
      effectiveWorkflow,
      effectiveClean,
    ),
  );

  // The dialog's first line says where the result goes (spec 2026-10-28
  // §7), so it never has to be asked. The backend's own answer —
  // `filing::default_destination`, read from SQLite through
  // `list_capture_folders` (Share to Jodd names its default the same way):
  // the Inbox, or the root when the account cannot create folders and has
  // none, or when the Inbox is going or blocked. Until it answers, the
  // capability decides, as default_destination does with no Inbox row yet.
  let defaultDestination = $state<{ account: string; path: string } | null>(null);
  $effect(() => {
    const acct = $currentAccount;
    if (!open || !acct) return;
    let live = true;
    invoke<{ default?: unknown }>('list_capture_folders', { accountId: acct })
      .then((r) => {
        if (live && typeof r?.default === 'string') defaultDestination = { account: acct, path: r.default };
      })
      .catch(() => {});
    return () => { live = false; };
  });
  const destinationLine = $derived.by(() => {
    if (destination === 'existing') {
      return targetNote ? `Adds to “${targetNote.title}”.` : 'Adds to the note you pick below.';
    }
    const acct = $currentAccount;
    const path = defaultDestination && defaultDestination.account === acct
      ? defaultDestination.path
      : canWrite(acct ? $capabilitiesByAccount[acct] : undefined, 'folders') ? 'Notes/Inbox' : 'Notes';
    return `Saves to ${viewName(null, path, '')} in ${accountDisplayById($accounts, acct)}.`;
  });

  function excludeUuids(): string[] {
    return [sourceNote?.uuid, targetNote?.uuid].filter((u): u is string => !!u);
  }

  // The stale analysis and its selection are cleared in the SAME turn as the
  // text change, and the fresh pair is assigned together (gotcha #28) —
  // never a selection that belongs to text no longer in the box.
  function scheduleAnalyze(text: string, isHtml: boolean) {
    clearTimeout(analyzeTimer);
    const seq = ++analyzeSeq;
    analysis = null;
    selectedUrls = [];
    transcribeMyself = false;
    const acct = get(currentAccount);
    if (!acct || !text.trim()) return;
    analyzeTimer = setTimeout(async () => {
      try {
        const res = await invoke<IngestAnalysis>('analyze_ingest_sources', {
          accountId: acct,
          text,
          isHtml,
          excludeUuids: excludeUuids(),
        });
        if (seq !== analyzeSeq) return;
        analysis = res;
        selectedUrls = initialSelection(res);
      } catch (e) {
        // Advisory, like check_duplicate_citations: Extract still works.
        console.warn('analyze_ingest_sources failed — no link sources offered', e);
      }
    }, ANALYZE_DEBOUNCE_MS);
  }

  $effect(() => {
    scheduleAnalyze(sourceText, sourceMode === 'existing');
  });

  // Debounced, sequence-guarded search — mirrors NoteList.svelte's
  // scheduleSearch (same 150ms delay, same stale-response guard).
  function scheduleTargetSearch(query: string) {
    clearTimeout(targetSearchTimer);
    const q = query.trim();
    if (!q) {
      targetResults = [];
      return;
    }
    const acct = get(currentAccount);
    if (!acct) {
      targetResults = [];
      return;
    }
    targetSearchTimer = setTimeout(async () => {
      const seq = ++targetSearchSeq;
      try {
        const res = await invoke<Note[]>('search_notes', {
          accountId: acct,
          label: null,
          query: q,
        });
        if (seq === targetSearchSeq) targetResults = res;
      } catch (e) {
        console.warn('target note search failed', e);
      }
    }, 150);
  }

  function pickTarget(note: Note) {
    targetNote = note;
    targetResults = [];
    targetQuery = note.title;
  }

  function setDestination(next: Destination) {
    destination = next;
    if (next === 'new') {
      targetNote = null;
      targetQuery = '';
      targetResults = [];
    }
  }

  function scheduleSourceNoteSearch(query: string) {
    clearTimeout(sourceNoteSearchTimer);
    const q = query.trim();
    if (!q) {
      sourceNoteResults = [];
      return;
    }
    const acct = get(currentAccount);
    if (!acct) {
      sourceNoteResults = [];
      return;
    }
    sourceNoteSearchTimer = setTimeout(async () => {
      const seq = ++sourceNoteSearchSeq;
      try {
        const res = await invoke<Note[]>('search_notes', {
          accountId: acct,
          label: null,
          query: q,
        });
        if (seq === sourceNoteSearchSeq) sourceNoteResults = res;
      } catch (e) {
        console.warn('source note search failed', e);
      }
    }, 150);
  }

  function pickSourceNote(note: Note) {
    sourceNote = note;
    sourceNoteResults = [];
    sourceNoteQuery = note.title;
    // Feed the picked note's body as source_text: link analysis reads its
    // hrefs (isHtml). The backend trims it before the provider sees it — a
    // Source block the note restates is dropped (`ai_source_text`), and a
    // failed run makes no copy of a note that is already saved.
    sourceText = note.body_html;
  }

  function setSourceMode(next: SourceMode) {
    sourceMode = next;
    if (next === 'paste') {
      sourceNote = null;
      sourceNoteQuery = '';
      sourceNoteResults = [];
      sourceText = '';
    }
  }

  // Resets every piece of form state, including the append-picker fields.
  // The modal component instance persists across open/close cycles, so
  // without this a user who switches to "Existing note…", picks a
  // target, then cancels would re-open the modal still in append mode with
  // the stale picked target shown.
  function resetForm() {
    discardPreview();
    pristine = { text: '', title: '' };
    sourceText = '';
    sourceIncomplete = false;
    titleOverride = '';
    errorMsg = '';
    consentAccount = '';
    workflow = 'extract';
    cleanTranscript = true;
    destination = 'new';
    targetNote = null;
    targetQuery = '';
    targetResults = [];
    sourceMode = 'paste';
    sourceNote = null;
    sourceNoteQuery = '';
    sourceNoteResults = [];
    analysis = null;
    selectedUrls = [];
    transcribeMyself = false;
    progressLog = [];
  }

  // The "set it up in Settings" link opens Settings › Speech to text. Settings
  // is a Modal opened later, so it stacks ON TOP of this one (and owns the
  // next Android Back); nothing here is closed or reset — the links the
  // person pasted are what they are about to ingest.
  function openAppSettings() {
    settingsRoute.set('speech');
  }

  // Settings closed with the modal still open: speech-to-text may now be set up
  // (or changed), so re-read the analysis. Only `stt_host` / `stt_insecure` are taken from it —
  // the text is unchanged, and re-running scheduleAnalyze would clear the
  // person's selection. Dropped if the text changed (a newer analysis owns it).
  let settingsWasOpen = false;
  $effect(() => {
    const settings = $settingsRoute !== null;
    untrack(() => {
      if (settingsWasOpen && !settings && open && analysis) void refreshSttHost();
      settingsWasOpen = settings;
    });
  });
  async function refreshSttHost() {
    const acct = get(currentAccount);
    const seq = analyzeSeq;
    if (!acct) return;
    try {
      const res = await invoke<IngestAnalysis>('analyze_ingest_sources', {
        accountId: acct,
        text: sourceText,
        isHtml: sourceMode === 'existing',
        excludeUuids: excludeUuids(),
      });
      if (seq === analyzeSeq && analysis) analysis = { ...analysis, stt_host: res.stt_host, stt_insecure: res.stt_insecure };
    } catch (e) {
      console.warn('analyze_ingest_sources failed — keeping the previous speech-to-text line', e);
    }
  }

  // RFC4122 v4 UUID generator (no external dep). The backend doesn't validate
  // the format — it's just used as a HashMap key — so any unique-per-call
  // string works. crypto.randomUUID is available in modern browsers / Tauri.
  function newRequestId(): string {
    return globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
  }

  // After a new note exists (Extract or Ingest): navigate to where the
  // backend filed it, paint it from cache, refresh tags, select it
  // (openCreatedNote), and run the post-Extract suggestions (extract filing
  // spec Decision 8).
  async function showCreatedNote(acct: string, created: ExtractedNote, parentRequestId: string) {
    retireCapture();
    const found = await openCreatedNote(acct, created);
    if (found) {
      // Not awaited: the modal is closed and the note is saved — nobody
      // waits on either call (spec data flow).
      void runPostExtractSuggestions(acct, created.uuid, found.title, found.body_html, parentRequestId);
    }
  }

  async function extract(skipDuplicateCheck = false) {
    const acct = get(currentAccount);
    if (!acct) {
      errorMsg = 'No account selected.';
      return;
    }
    if (!sourceText.trim()) {
      errorMsg = 'Paste some source text first.';
      return;
    }
    busy = true;
    errorMsg = '';
    duplicateWarnings = [];
    requestId = newRequestId();
    receiptRequestId = requestId;
    const generation = ++previewGeneration;
    if (effectiveWorkflow === 'action_items') previewAccount = acct;
    if (destination === 'existing' && !targetNote) {
      errorMsg = 'Pick a note to append to, or switch to "New note".';
      busy = false;
      requestId = '';
      return;
    }
    if (sourceMode === 'existing' && !sourceNote) {
      errorMsg = 'Pick a source note, or switch to "Paste text".';
      busy = false;
      requestId = '';
      return;
    }

    if (!skipDuplicateCheck) {
      try {
        const excludeUuid = destination === 'existing' ? (targetNote?.uuid ?? null) : null;
        const warnings = await invoke<
          { url: string; existing_note_uuid: string; existing_note_title: string }[]
        >('check_duplicate_citations', { accountId: acct, sourceText, excludeUuid });
        if (warnings.length > 0) {
          duplicateWarnings = warnings;
          busy = false;
          requestId = '';
          return;
        }
      } catch (e) {
        // Dedup check is advisory — a transient failure must not block
        // extraction. Log and proceed as if no duplicates were found.
        console.warn('check_duplicate_citations failed — proceeding without dedup check', e);
      }
    }

    try {
      if (effectiveWorkflow === 'action_items') {
        if (disposed || generation !== previewGeneration || get(currentAccount) !== acct) return;
        const result = await invoke<ActionItemsPreview>('preview_action_items', {
          accountId: acct, sourceText, sourceIncomplete, sourceUuid: sourceMode === 'existing' ? sourceNote?.uuid : null,
          targetUuid: destination === 'existing' ? targetNote?.uuid : null,
          titleOverride: titleOverride.trim() || null, requestId,
        });
        if (disposed || generation !== previewGeneration || get(currentAccount) !== acct) {
          void invoke('discard_action_items', { aiResultId: result.ai_result_id }).catch(() => {});
          return;
        }
        actionPreview = result;
        return;
      }
      // Backend command takes snake_case fields in Rust; Tauri's serde
      // bridge expects camelCase on the JS side (matches every other
      // invoke in this codebase — see AccountSettings, NoteContextMenu).
      if (destination === 'existing' && targetNote) {
        // Capture the label before the invoke — targetNote gets reset to
        // null right after a successful append, and the note's own body_html
        // in the `notes` store won't reflect the new content until the
        // lookup below, so this is the only reliable source for navigation.
        const targetLabel = targetNote.label;
        const targetUuid =
          effectiveWorkflow === 'extract'
            ? await invoke<string>('append_extract_note', {
                accountId: acct,
                targetUuid: targetNote.uuid,
                sourceText,
                source: aiSourceFor(sourceMode, sourceNote),
                requestId,
              })
            : await invoke<string>('append_llm_workflow_note', {
                accountId: acct,
                workflow: backendWorkflow(effectiveWorkflow as Exclude<ModalWorkflow, 'extract'>),
                targetUuid: targetNote.uuid,
                sourceText,
                source: aiSourceFor(sourceMode, sourceNote),
                requestId,
              });
        void refreshUnreviewed([acct]); // R15: an AI write makes a note unreviewed with no remote-changed behind it.
        retireCapture();
        closeIntoResult(targetLabel);
        sourceText = '';
        titleOverride = '';
        targetNote = null;
        targetQuery = '';
        destination = 'new';
        // Navigate to wherever the target note lives (its own folder, not
        // necessarily wherever a new-note extract would land) and refresh
        // tags, mirroring the new-note path below.
        navigateToFolder(targetLabel);
        // The selectedFolder.set() inside navigateToFolder() is a no-op when
        // targetLabel already equals the current folder (Svelte's
        // safe_not_equal skips subscribers on an unchanged value) — the very
        // likely case when appending into a note in the folder the user is
        // already viewing.
        // Don't rely on the folder-watcher side effect alone: explicitly
        // re-fetch and merge this folder's rows so the `find()` below sees
        // the freshly-appended body_html instead of a stale snapshot.
        try {
          const cached = await invoke<Note[]>('list_cached_notes_in_folder', {
            accountId: acct,
            path: targetLabel,
          });
          notes.update((ns) => {
            const others = ns.filter(
              (n) => !(n.account_id === acct && n.label === targetLabel),
            );
            return [...others, ...cached];
          });
        } catch (e) {
          console.warn('post-append cache paint failed', e);
        }
        try {
          const rows = await invoke<{ uuid: string; tag: string }[]>(
            'list_note_tags',
            { accountId: acct },
          );
          setAccountNoteTags(acct, rows);
        } catch (e) {
          console.warn('post-append tag refresh failed', e);
        }
        const found = get(notes).find((n: Note) => n.account_id === acct && n.uuid === targetUuid);
        if (found) {
          selectedNote.set(found);
          indexUpsertOnSave(acct, null, found.id, found.label);
          await runAutoLinkSuggestions(acct, targetUuid, found.title, found.body_html, receiptRequestId);
        }
        busy = false;
        requestId = '';
        return;
      }

      const created =
        effectiveWorkflow === 'extract'
          ? await invoke<ExtractedNote>('extract_note', {
              accountId: acct,
              sourceText,
              source: aiSourceFor(sourceMode, sourceNote),
              titleOverride: titleOverride.trim() || null,
              requestId,
            })
          : await invoke<ExtractedNote>('run_llm_workflow', {
              accountId: acct,
              workflow: backendWorkflow(effectiveWorkflow as Exclude<ModalWorkflow, 'extract'>),
              sourceText,
              source: aiSourceFor(sourceMode, sourceNote),
              titleOverride: titleOverride.trim() || null,
              requestId,
            });
      void refreshUnreviewed([acct]); // R15: an AI write makes a note unreviewed with no remote-changed behind it.

      // Close + reset before navigation so the modal doesn't flash
      // while we repaint.
      closeIntoResult(created.label);
      sourceText = '';
      titleOverride = '';

      await showCreatedNote(acct, created, receiptRequestId);
    } catch (e) {
      const msg = String(e);
      if (msg === 'cancelled' || msg.endsWith(': cancelled')) {
        // Cancel landed before any DB write — source still in textarea,
        // user can retry or paste new content. No "error" UI state needed.
        errorMsg = '';
      } else {
        // Preserve sourceText so the user can retry without re-pasting.
        errorMsg = msg;
        consentAccount = offerConsentFor(acct);
      }
    } finally {
      busy = false;
      requestId = '';
    }
  }

  function continueAnyway() {
    duplicateWarnings = [];
    extract(true);
  }

  // Privacy PR3: answering the refusal is the consent. Optimistic, with
  // rollback only while the user is still on that account and nothing newer
  // has been shown.
  async function allowAi() {
    const acct = consentAccount;
    if (!acct) return;
    const previous = errorMsg;
    errorMsg = '';
    consentAccount = '';
    try {
      await allowAiForAccount(acct);
    } catch (e) {
      if (!errorMsg && get(currentAccount) === acct) {
        errorMsg = `${previous}\nCould not allow AI for this account: ${e}`;
        consentAccount = acct;
      }
    }
  }

  async function ingest() {
    const acct = get(currentAccount);
    if (!acct || selectedUrls.length === 0) return;
    busy = true;
    errorMsg = '';
    progressLog = [];
    requestId = newRequestId();
    receiptRequestId = requestId;
    const onProgress = new Channel<IngestProgress>();
    onProgress.onmessage = (p) => {
      progressLog = [...progressLog, progressLine(p)];
    };
    try {
      const created = await invoke<ExtractedNote>('ingest_sources', {
        accountId: acct,
        urls: selectedUrls,
        contextText: analysis?.context_text ?? '',
        titleOverride: titleOverride.trim() || null,
        requestId,
        onProgress,
        mode: ingestModeFor(effectiveWorkflow, effectiveClean),
        source: aiSourceFor(sourceMode, sourceNote),
        // Sent only when ticked: absent means "use the platform's own transcript".
        ...(transcribeAudio ? { transcribeAudio: true } : {}),
      });
      void refreshUnreviewed([acct]); // R15: an AI write makes a note unreviewed with no remote-changed behind it.
      closeIntoResult(created.label);
      resetForm();
      await showCreatedNote(acct, created, receiptRequestId);
    } catch (e) {
      const msg = String(e);
      // Cancel wrote nothing; anything else keeps the input for a retry.
      errorMsg = msg === 'cancelled' || msg.endsWith(': cancelled') ? '' : msg;
      consentAccount = offerConsentFor(acct);
    } finally {
      busy = false;
      requestId = '';
      progressLog = [];
    }
  }

  // Cancel the in-flight extraction, if any. Best-effort: a stale request_id
  // (extract already completed) just returns Ok(false) on the backend.
  async function cancelInFlight() {
    if (effectiveWorkflow === 'action_items') previewGeneration++;
    if (!busy || !requestId) return;
    try {
      await invoke('cancel_extraction', { requestId });
    } catch (e) {
      console.warn('cancel_extraction invoke failed', e);
    }
  }

  // Every Modal close path (Esc, backdrop, ✕/‹, Android Back) asks this
  // first. While a run is in flight a close attempt cancels the run and keeps
  // the modal — the source stays for a retry; the next attempt closes.
  function beforeClose(): boolean {
    if (!busy) return true;
    void cancelInFlight();
    return false;
  }

  // The person's own unsaved input: typed or picked source text, a typed
  // title, or a generated meeting-actions preview. What a capture handed over
  // is not (`pristine`). Closing with any of it asks first.
  const dirty = $derived(
    (sourceText.trim() !== '' && sourceText !== pristine.text) ||
      (titleOverride.trim() !== '' && titleOverride !== pristine.title) ||
      !!actionPreview,
  );

  let modal: { closeInto(pane: Pane, title?: string): boolean } | undefined = $state();
  // Set only while closeInto runs, so onModalClose knows the success path
  // resets what it owns (it keeps the workflow, as before).
  let closingInto = false;

  function onModalClose() {
    open = false;
    if (!closingInto) resetForm();
  }

  // A finished run shows its note: the modal closes INTO that destination
  // (Modal.closeInto — never a plain close followed by navigation). Filed in
  // another folder, the run lands on that folder's list first (App's folder
  // watcher would push it anyway) and App's selection watcher opens the note
  // over it; in the folder already shown, it opens the note directly.
  function closeIntoResult(label: string) {
    const pane: Pane = get(selectedFolder) === label ? 'note' : 'list';
    closingInto = true;
    try {
      // Landing on the list: name it (the folder the run filed into) for the
      // Back label of the note App then opens over it.
      if (!modal?.closeInto(pane, pane === 'list' ? viewName(null, label, '') : undefined)) open = false;
    } finally {
      closingInto = false;
    }
  }

  const primaryLabel = $derived(
    busy
      ? 'Working…'
      : ingesting
        ? `Make note from ${selectedUrls.length} ${selectedUrls.length === 1 ? 'link' : 'links'}`
        : effectiveWorkflow === 'action_items'
          ? 'Preview action items'
          : destination === 'existing'
            ? 'Append'
            : 'Make note',
  );
</script>

{#if open}
  <Modal
    bind:this={modal}
    title={DIALOG_TITLE}
    size="lg"
    {dirty}
    onbeforeclose={beforeClose}
    onclose={onModalClose}
    primary={actionPreview
      ? undefined
      : {
          label: primaryLabel,
          onclick: () => (ingesting ? ingest() : extract()),
          disabled: !sourceText.trim() || (destination === 'existing' && !targetNote) || (sourceMode === 'existing' && !sourceNote) || sourceHidden,
          busy,
        }}
    footer={busy ? cancelRun : undefined}
  >
    {#if actionPreview}
      <ActionItemsReview preview={actionPreview} {busy} onapply={applyActions} ondiscard={discardPreview} />
    {:else}
    <!-- Spec 2026-10-28 §7: what a person decides, in the order they decide
         it — where it goes (said, not asked), what to make, from what, then
         the options for that. "Add to" comes last and only when it is a
         real choice. -->
    <p class="hint destination-line">{destinationLine}</p>

    <div class="field-heading" id="make-heading">What to make</div>
    <div class="destination-toggle" role="radiogroup" aria-labelledby="make-heading">
      {#each WORKFLOW_OPTIONS as opt (opt.value)}
        <button
          type="button"
          class:active={effectiveWorkflow === opt.value}
          onclick={() => setWorkflow(opt.value)}
          disabled={busy || (ingesting && opt.value === 'action_items')}
        >{opt.label}</button>
      {/each}
    </div>
    <p class="option-line">{WORKFLOW_HINTS[effectiveWorkflow]}.</p>
    {#if ingesting}
      <p class="ingest-note">Action items work on pasted text only.</p>
    {/if}
    {#if keywordsHint}
      <p class="ingest-note">{KEYWORDS_HINT}</p>
    {/if}

    {#if sourceMode === 'paste'}
      <label>
        Source
        <textarea
          bind:value={sourceText}
          rows="12"
          disabled={busy}
          placeholder="Paste text or links…"
        ></textarea>
      </label>
      <button type="button" class="link-button" onclick={() => setSourceMode('existing')} disabled={busy}>Use an existing note instead</button>
    {:else}
      <label>
        Source note
        <input
          type="text"
          bind:value={sourceNoteQuery}
          oninput={() => { sourceNote = null; scheduleSourceNoteSearch(sourceNoteQuery); }}
          disabled={busy}
          placeholder="Search notes by title or content..."
        />
      </label>
      {#if sourceNote}
        <p class="target-picked">Source: <strong>{sourceNote.title}</strong> ({sourceNote.label})</p>
      {:else if sourceNoteResults.length > 0}
        <ul class="target-results">
          {#each sourceNoteResults as r (r.uuid)}
            <li>
              <button type="button" onclick={() => pickSourceNote(r)}>
                {r.title} <span class="target-folder">{r.label}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      {#if sourceHidden}<p class="error">{AI_HIDDEN_HINT}</p>{/if}
      <button type="button" class="link-button" onclick={() => setSourceMode('paste')} disabled={busy}>Paste text instead</button>
    {/if}

    {#if analysis && supportedCount > 0}
      <section class="ingest-sources" aria-label="Sources from links">
        <h3>Sources from links</h3>
        {#if destination === 'existing'}
          <p class="ingest-note">Links always make a new note. Switch to “New note” to use them.</p>
        {:else if supportedCount > MAX_URLS_PER_INGEST}
          <p class="ingest-note">At most {MAX_URLS_PER_INGEST} links can be used at once.</p>
        {/if}
        <ul class="source-list">
          {#each analysis.sources as s (s.url)}
            <li>
              <label class="source-row">
                <input
                  type="checkbox"
                  checked={selectedUrls.includes(s.url)}
                  disabled={busy || !s.supported || destination === 'existing'}
                  onchange={() => (selectedUrls = toggleUrl(selectedUrls, s.url))}
                />
                <span class="source-kind">{sourceKindLabel(s.kind)}</span>
                <span class="source-url">{s.url}</span>
                {#if s.duplicate_owner}
                  <span class="dup-badge">already in “{s.duplicate_owner.title}”</span>
                {/if}
                {#if s.reason}
                  <span class="source-reason">{s.reason}</span>
                {/if}
              </label>
            </li>
          {/each}
        </ul>
        {#if mayTranscribe}
          {#if sttHost}
            <p class="ingest-note audio-line" data-testid="audio-destination">Audio from these clips will be sent to {sttHost}{sttInsecure ? ' (not encrypted)' : ''}.</p>
          {:else}
            <p class="ingest-note audio-line" data-testid="audio-unconfigured">
              Spoken content needs speech-to-text — <button type="button" class="settings-link" disabled={busy} onclick={openAppSettings}>set it up in Settings</button>. Captions will be used instead.
            </p>
          {/if}
        {/if}
        {#if offerOwnTranscription}
          <label class="own-transcription">
            <input type="checkbox" bind:checked={transcribeMyself} disabled={busy} />
            Transcribe the audio myself
          </label>
        {/if}
        {#if progressLog.length > 0}
          <ol class="ingest-progress" aria-live="polite">
            {#each progressLog as line, i (i)}
              <li>{line}</li>
            {/each}
          </ol>
        {/if}
      </section>
    {/if}

    {#if effectiveWorkflow === 'transcript'}
      <label class="incomplete-source" title={ingesting ? '' : 'Pasted text is always cleaned — a raw copy would just duplicate it.'}>
        <input
          type="checkbox"
          class="clean-transcript"
          checked={effectiveClean}
          disabled={busy || !ingesting}
          onchange={(e) => (cleanTranscript = e.currentTarget.checked)}
        />Clean (rewrite with AI, keep everything)
      </label>
    {/if}
    {#if effectiveWorkflow === 'action_items'}
      <label class="incomplete-source"><input type="checkbox" bind:checked={sourceIncomplete} disabled={busy} />Source is incomplete — show unresolved information only</label>
    {/if}

    {#if destination === 'new'}
      <label>
        Title (optional)
        <input
          type="text"
          bind:value={titleOverride}
          disabled={busy}
          placeholder="Made from the result"
        />
      </label>
    {/if}

    <!-- Hidden while ticked links will run: they always make a new note, so
         there is nothing to choose. With "Existing note" already chosen it
         stays, so the person can switch back. -->
    {#if !ingesting}
      <div class="add-to">
        <span class="field-heading" id="add-to-heading">Add to</span>
        <div class="destination-toggle" role="radiogroup" aria-labelledby="add-to-heading">
          <button
            type="button"
            class:active={destination === 'new'}
            disabled={busy}
            onclick={() => setDestination('new')}
          >New note</button>
          <button
            type="button"
            class:active={destination === 'existing'}
            disabled={busy}
            onclick={() => setDestination('existing')}
          >Existing note…</button>
        </div>
      </div>
      {#if destination === 'existing'}
        <label>
          Target note
          <input
            type="text"
            bind:value={targetQuery}
            oninput={() => { targetNote = null; scheduleTargetSearch(targetQuery); }}
            disabled={busy}
            placeholder="Search notes by title or content..."
          />
        </label>
        {#if targetNote}
          <p class="target-picked">Appending to: <strong>{targetNote.title}</strong> ({targetNote.label})</p>
        {:else if targetResults.length > 0}
          <ul class="target-results">
            {#each targetResults as r (r.uuid)}
              <li>
                <button type="button" onclick={() => pickTarget(r)}>
                  {r.title} <span class="target-folder">{r.label}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    {/if}

    {#if duplicateWarnings.length > 0}
      <div class="dup-warning">
        {#each duplicateWarnings as w (w.url)}
          <p>
            Already used <strong>{w.url}</strong> in
            <em>{w.existing_note_title}</em>.
          </p>
        {/each}
        <button type="button" onclick={continueAnyway}>Continue anyway</button>
      </div>
    {/if}

    {/if}
    {#if errorMsg}
      <div class="error">{errorMsg}</div>
    {/if}
    {#if consentAccount && isConsentRefusal(errorMsg)}
      <button type="button" onclick={allowAi}>{allowAiLabel(accountDisplayById($accounts, consentAccount))}</button>
    {/if}

    <AiReceipts requestId={receiptRequestId} />
  </Modal>
{/if}

<!-- While a run is in flight: propagates to cancel_extraction(requestId),
     which kills the in-flight provider call; the source stays for a retry.
     Idle, the Modal header close button is the close control. -->
{#snippet cancelRun()}
  <button type="button" class="cancel-run" onclick={() => void cancelInFlight()}>Stop</button>
{/snippet}

{#if linkSuggestionsOpen}
  <LinkSuggestionsModal
    accountId={$currentAccount ?? ''}
    {proposedAppends}
    onClose={() => { linkSuggestionsOpen = false; proposedAppends = []; }}
  />
{/if}

<style>
  .incomplete-source { display: flex; align-items: center; gap: 8px; }
  .incomplete-source input[type='checkbox'] { width: auto; flex: none; }
  .hint { color: var(--text-muted); font-size: var(--size); margin: 0 0 16px; }
  /* Spec 2026-10-28 §7: a heading over each choice, one line under it. */
  .field-heading { margin: 0 0 var(--space-1-5); font-size: var(--size); color: var(--text-secondary); }
  .option-line { margin: 0 0 var(--space-2); color: var(--text-muted); font-size: var(--size-sm); }
  .add-to { margin: var(--space-3) 0 0; }
  .link-button {
    background: none;
    border: none;
    padding: 0;
    margin: 0 0 var(--space-2);
    color: var(--accent-action);
    font: inherit;
    font-size: var(--size-sm);
    text-decoration: underline;
    cursor: pointer;
  }
  .link-button:disabled { color: var(--text-muted); cursor: default; }
  label { display: block; margin: 12px 0; font-size: var(--size); color: var(--text-secondary); }
  .destination-toggle {
    display: flex;
    gap: 8px;
    margin: 12px 0;
  }
  .destination-toggle button {
    flex: 1;
    padding: 8px;
    font-size: var(--size-sm);
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface-panel);
    cursor: pointer;
  }
  .destination-toggle button.active {
    border-color: var(--accent-action);
    background: var(--accent-wash);
    font-weight: 600;
  }
  .target-picked {
    font-size: var(--size);
    color: var(--text);
    line-height: var(--leading);
    margin: 8px 0;
  }
  .target-results {
    list-style: none;
    margin: 4px 0 12px;
    padding: 0;
    max-height: 160px;
    overflow-y: auto;
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
  }
  .target-results li button {
    display: block;
    width: 100%;
    text-align: left;
    padding: 6px 8px;
    background: none;
    border: none;
    cursor: pointer;
    font-size: var(--size-sm);
    line-height: var(--leading);
  }
  .target-results li button:hover {
    background: var(--surface-hover);
  }
  .target-folder {
    color: var(--text-muted);
    font-size: var(--size-xs);
    line-height: var(--leading);
  }
  textarea, input {
    width: 100%; padding: 8px; font: inherit; box-sizing: border-box;
    margin-top: 4px;
    /* Stated, never inherited: the UA stylesheet beats inheritance for form
       controls, so an unset background is the UA's white regardless of theme —
       measured 1.25:1 against --text in dark mode. */
    background: var(--surface-panel);
    color: var(--text);
  }
  textarea { font-family: var(--font-mono); font-size: var(--size-sm); }
  .error {
    color: var(--danger); padding: 8px; background: var(--danger-wash); border-radius: 4px;
    margin: 12px 0; white-space: pre-wrap;
  }
  .dup-warning {
    color: var(--accent-action); padding: 8px; background: var(--warn-wash); border-radius: 4px;
    margin: 12px 0;
  }
  .dup-warning p { margin: 0 0 6px; }
  .dup-warning button {
    background: var(--surface-panel); border: 1px solid var(--accent); border-radius: 4px;
    padding: 4px 10px; cursor: pointer;
  }
  .cancel-run {
    font: inherit; font-size: var(--size-sm); color: var(--text); cursor: pointer;
    padding: var(--space-1) var(--space-3); border-radius: var(--radius);
    border: 1px solid var(--border); background: var(--surface-panel);
  }
  .cancel-run:hover { background: var(--surface-hover); }
  .ingest-sources { margin: 12px 0; padding: 8px; border: 1px solid var(--border); border-radius: 4px; }
  .ingest-sources h3 { margin: 0 0 6px; font-size: var(--size); }
  .ingest-note { margin: 0 0 6px; color: var(--text-muted); font-size: var(--size-sm); }
  .settings-link { background: none; border: none; padding: 0; color: var(--accent-action); cursor: pointer; font: inherit; text-decoration: underline; }
  .own-transcription { display: flex; align-items: center; gap: 6px; margin: 0 0 6px; font-size: var(--size-sm); }
  .source-list { list-style: none; margin: 0; padding: 0; }
  .source-row { display: flex; flex-wrap: wrap; gap: 6px; align-items: baseline; margin: 4px 0; font-size: var(--size-sm); color: var(--text); }
  /* The modal's `textarea, input { width: 100% }` also matches this checkbox,
     which then fills its own flex line and renders centred above the URL
     (seen on an Android phone). The row's checkbox sizes to itself. */
  .source-row input[type='checkbox'] { width: auto; padding: 0; margin: 0; flex: none; align-self: center; }
  .source-kind { color: var(--text-muted); }
  .source-url { overflow-wrap: anywhere; }
  .dup-badge { background: var(--warn-wash); color: var(--accent-action); border-radius: 4px; padding: 0 6px; }
  .source-reason { color: var(--text-muted); }
  .ingest-progress { margin: 8px 0 0; padding-left: 20px; font-size: var(--size-sm); color: var(--text-secondary); }
</style>
