<script lang="ts">
  // Restore from a backup folder (spec 2026-10-23 backup-restore §6.1):
  // account → version → what → where → preview → restore. One dialog, one
  // step at a time; the Modal's primary button is the step's Next.
  //
  // Local-first: this component keeps only its own working state. The restore
  // writes SQLite on the Rust side and the backend emits `remote-changed`, so
  // nothing here touches the notes store after an awaited call.
  import { untrack } from 'svelte';
  import {
    accountDisplay,
    accountDisplayById,
    accounts,
    activeAccounts,
    backendLabel,
    canWriteAccount,
    capabilitiesByAccount,
  } from '../../stores/notes';
  import {
    deleteBackupVersion,
    listBackupSlugs,
    listBackupVersions,
    restoreApply,
    restorePreview,
  } from '../../backup/api';
  import type {
    ExistingChoice,
    Loss,
    ManifestNote,
    Manifest,
    Placement,
    PlannedNote,
    RestorePlan,
    RestoreReport,
    RestoreRequest,
    VersionEntry,
  } from '../../backup/api';
  import { friendlyBackupMessage } from '../../backup/state';
  import Modal from '../Modal.svelte';
  import { settingsRoute } from '../../stores/ui';

  // `initialAccountId`: opened from one account's row, so start on that
  // account's backups (spec §8: restore lives beside Back up now too).
  let {
    destinationId,
    open,
    onclose,
    initialAccountId = null,
  }: { destinationId: string; open: boolean; onclose: () => void; initialAccountId?: string | null } = $props();

  type Step = 'account' | 'version' | 'what' | 'where' | 'preview' | 'done';
  type SlugEntry = { slug: string; versions: VersionEntry[]; error: string | null };

  let modal: { requestClose(): Promise<boolean> } | undefined = $state();
  let step = $state<Step>('account');
  let loading = $state(false);
  let error = $state<string | null>(null);

  // 1 · account
  let slugs = $state<SlugEntry[]>([]);
  let chosenSlug = $state<string | null>(null);
  // 2 · version
  let chosenStamp = $state<string | null>(null);
  let confirmDelete = $state<string | null>(null);
  let deleting = $state(false);
  // 3 · what
  let selected = $state<Set<string>>(new Set());
  let search = $state('');
  let collapsed = $state<Set<string>>(new Set());
  // 4 · where
  let targetAccount = $state<string | null>(null);
  let placementKind = $state<'original' | 'new'>('original');
  let existing = $state<ExistingChoice>('keepBoth');
  // 5 · preview, 6 · done
  let plan = $state<RestorePlan | null>(null);
  let previewing = $state(false);
  let previewError = $state<string | null>(null);
  let applying = $state(false);
  let report = $state<RestoreReport | null>(null);

  let loadToken = 0;
  let previewToken = 0;

  const slugEntry = $derived(slugs.find((s) => s.slug === chosenSlug) ?? null);
  const version = $derived(slugEntry?.versions.find((v) => v.stamp === chosenStamp) ?? null);
  const manifest = $derived<Manifest | null>(version?.manifest ?? null);

  // A dialog that reopens starts over; the effect depends on `open` and the
  // destination only (everything `start` reads or writes is untracked).
  $effect(() => {
    if (open) {
      const dest = destinationId;
      untrack(() => void start(dest));
    }
  });

  async function start(dest: string) {
    const token = ++loadToken;
    previewToken++;
    step = 'account';
    error = null;
    slugs = [];
    chosenSlug = null;
    chosenStamp = null;
    confirmDelete = null;
    plan = null;
    previewError = null;
    report = null;
    applying = false;
    loading = true;
    try {
      const names = await listBackupSlugs(dest);
      const entries = await Promise.all(names.map((slug) => loadSlug(dest, slug)));
      if (token !== loadToken) return;
      slugs = entries;
      const own = initialAccountId
        ? entries.find((s) => s.versions[0]?.manifest.accountId === initialAccountId)
        : undefined;
      chosenSlug = own?.slug ?? entries.find((s) => s.versions.length > 0)?.slug ?? null;
      // Opened for one account that has backups here: its versions are the first question.
      if (own) {
        chosenStamp = own.versions[0]?.stamp ?? null;
        step = 'version';
      }
    } catch (e) {
      if (token === loadToken) error = friendlyBackupMessage(String(e));
    } finally {
      if (token === loadToken) loading = false;
    }
  }

  async function loadSlug(dest: string, slug: string): Promise<SlugEntry> {
    try {
      const versions = await listBackupVersions(dest, slug);
      return { slug, versions: [...versions].sort((a, b) => (a.stamp < b.stamp ? 1 : -1)), error: null };
    } catch (e) {
      return { slug, versions: [], error: String(e) };
    }
  }

  // ---- formatting ---------------------------------------------------------

  const when = (ms: number) => new Date(ms).toLocaleString();
  const TRIGGERS = { manual: 'Manual', scheduled: 'Scheduled', 'pre-restore': 'Before a restore' } as const;
  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  }
  const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
  /** Notes this version holds only partially (spec §11), shown before it is picked. */
  const partialCount = (v: VersionEntry) => v.manifest.notes.filter((n) => n.partial).length;
  function localDate(d = new Date()): string {
    const p = (n: number) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  }

  // ---- 3 · the tree -------------------------------------------------------

  type TreeFolder = { path: string; name: string; folders: TreeFolder[]; notes: ManifestNote[] };
  type Row =
    | { kind: 'folder'; node: TreeFolder; depth: number; uuids: string[] }
    | { kind: 'note'; note: ManifestNote; depth: number };

  function buildTree(m: Manifest): TreeFolder {
    const root: TreeFolder = { path: '', name: '', folders: [], notes: [] };
    const byPath = new Map<string, TreeFolder>([['', root]]);
    const ensure = (path: string): TreeFolder => {
      const hit = byPath.get(path);
      if (hit) return hit;
      const cut = path.lastIndexOf('/');
      const parent = ensure(cut < 0 ? '' : path.slice(0, cut));
      const node: TreeFolder = { path, name: cut < 0 ? path : path.slice(cut + 1), folders: [], notes: [] };
      parent.folders.push(node);
      byPath.set(path, node);
      return node;
    };
    for (const f of m.folders) if (f) ensure(f);
    for (const n of m.notes) ensure(n.folder).notes.push(n);
    const sort = (f: TreeFolder) => {
      f.folders.sort((a, b) => a.name.localeCompare(b.name));
      f.notes.sort((a, b) => a.title.localeCompare(b.title));
      f.folders.forEach(sort);
    };
    sort(root);
    return root;
  }

  const query = $derived(search.trim().toLowerCase());
  const matches = (n: ManifestNote) => !query || n.title.toLowerCase().includes(query);
  const visibleNotes = (f: TreeFolder): ManifestNote[] => [
    ...f.notes.filter(matches),
    ...f.folders.flatMap(visibleNotes),
  ];

  const rows = $derived.by<Row[]>(() => {
    if (!manifest) return [];
    const out: Row[] = [];
    const walk = (f: TreeFolder, depth: number) => {
      for (const note of f.notes.filter(matches)) out.push({ kind: 'note', note, depth });
      for (const child of f.folders) {
        const uuids = visibleNotes(child).map((n) => n.uuid);
        if (query && uuids.length === 0) continue;
        out.push({ kind: 'folder', node: child, depth, uuids });
        // A search shows every match, however deep.
        if (query || !collapsed.has(child.path)) walk(child, depth + 1);
      }
    };
    walk(buildTree(manifest), 0);
    return out;
  });
  // Select all / none act on what the search leaves visible, collapsed or not.
  const shownUuids = $derived(manifest ? manifest.notes.filter(matches).map((n) => n.uuid) : []);

  function setMany(uuids: string[], on: boolean) {
    const next = new Set(selected);
    for (const u of uuids) {
      if (on) next.add(u);
      else next.delete(u);
    }
    selected = next;
  }
  function folderState(uuids: string[]): { checked: boolean; indeterminate: boolean } {
    const n = uuids.filter((u) => selected.has(u)).length;
    return { checked: uuids.length > 0 && n === uuids.length, indeterminate: n > 0 && n < uuids.length };
  }
  function toggleCollapsed(path: string) {
    const next = new Set(collapsed);
    if (!next.delete(path)) next.add(path);
    collapsed = next;
  }
  function indeterminate(node: HTMLInputElement, value: boolean) {
    node.indeterminate = value;
    return { update: (v: boolean) => (node.indeterminate = v) };
  }

  // ---- 4 · where ----------------------------------------------------------

  const targets = $derived($activeAccounts.filter((a) => canWriteAccount($capabilitiesByAccount[a.id])));
  const sameAccount = $derived(!!manifest && targetAccount === manifest.accountId);
  /** The name the owner knows an account by; the raw id only for one this device lacks. */
  const nameOf = (accountId: string) => accountDisplayById($accounts, accountId);
  // Live pass 2026-10-24: the folder took the account's internal id. A folder
  // segment cannot hold / and some backends refuse \ : * ? " < > | as well.
  const newFolderName = $derived(
    `Restored ${manifest ? nameOf(manifest.accountId) : ''} ${localDate()}`.replace(/[\\/:*?"<>|]/g, '-'),
  );

  /** Leaves the restore for Settings › Accounts; restore again once the account is there. */
  function addAccount() {
    settingsRoute.set('accounts');
    void modal?.requestClose();
  }

  function buildRequest(): RestoreRequest {
    const placement: Placement = placementKind === 'new' ? { underNewFolder: newFolderName } : 'original';
    return {
      destinationId,
      slug: chosenSlug ?? '',
      stamp: chosenStamp ?? '',
      sourceUuids: (manifest?.notes ?? []).filter((n) => selected.has(n.uuid)).map((n) => n.uuid),
      targetAccount: targetAccount ?? '',
      placement,
      // Replace and Skip only mean something against the source account's own
      // notes; a cross-account restore always lands beside what is there.
      existing: sameAccount ? existing : 'keepBoth',
    };
  }

  // ---- 5 · preview --------------------------------------------------------

  const ACTION_TEXT: Record<string, [string, string]> = {
    create: ['note will be created', 'notes will be created'],
    keepBoth: ['note will be restored as a copy beside the existing one', 'notes will be restored as copies beside the existing ones'],
    replace: ['note will replace the existing one', 'notes will replace the existing ones'],
    skipExisting: ['note is skipped because it already exists', 'notes are skipped because they already exist'],
    skipAlreadyRestored: ['note was already restored from this backup and is skipped', 'notes were already restored from this backup and are skipped'],
    skipContentMatch: ['note is already in this account with the same text and is skipped', 'notes are already in this account with the same text and are skipped'],
    refuse: ["note can't be restored", "notes can't be restored"],
  };
  const ACTION_ORDER = ['create', 'keepBoth', 'replace', 'skipExisting', 'skipAlreadyRestored', 'skipContentMatch', 'refuse'];

  const counts = $derived.by(() => {
    const by = new Map<string, number>();
    for (const n of plan?.notes ?? []) by.set(n.action.kind, (by.get(n.action.kind) ?? 0) + 1);
    return ACTION_ORDER.filter((k) => by.has(k)).map((k) => {
      const n = by.get(k)!;
      return { kind: k, n, text: ACTION_TEXT[k][n === 1 ? 0 : 1] };
    });
  });
  const refused = $derived(
    (plan?.notes ?? []).flatMap((n: PlannedNote) => (n.action.kind === 'refuse' ? [{ title: n.title, reason: n.action.reason }] : [])),
  );
  const writes = $derived(
    (plan?.notes ?? []).filter((n) => ['create', 'keepBoth', 'replace'].includes(n.action.kind)).length,
  );
  const titleOf = (uuid: string) => manifest?.notes.find((n) => n.uuid === uuid)?.title ?? uuid;

  const LOSS_SECTIONS: { kind: Loss['kind']; heading: string }[] = [
    { kind: 'folderFlattened', heading: 'Folders' },
    { kind: 'attachment', heading: 'Attachments' },
    { kind: 'pin', heading: 'Pins' },
    { kind: 'tagsAsKeywords', heading: 'Tags' },
    { kind: 'partial', heading: 'Backed-up parts' },
  ];
  function lossText(l: Loss): string {
    switch (l.kind) {
      case 'folderFlattened': return `${l.from} will be placed in the top folder — this account has no folders`;
      case 'attachment': return `${l.filename} can't be stored in this account`;
      case 'pin': return "Pin won't carry over";
      case 'tagsAsKeywords': return 'Tags will be added as a Keywords line';
      case 'partial': return `Backed up without the parts Jodd can't read (${l.reason})`;
    }
  }
  /** Losses grouped by kind, then by their text, each with the notes it touches. */
  function lossGroups(items: { title: string; loss: Loss }[]) {
    return LOSS_SECTIONS.map((s) => {
      const byText = new Map<string, string[]>();
      for (const it of items) {
        if (it.loss.kind !== s.kind) continue;
        const text = lossText(it.loss);
        byText.set(text, [...(byText.get(text) ?? []), it.title]);
      }
      return { heading: s.heading, entries: [...byText].map(([text, titles]) => ({ text, titles })) };
    }).filter((g) => g.entries.length > 0);
  }
  const noteList = (titles: string[]) =>
    titles.length <= 3 ? titles.join(', ') : `${titles.slice(0, 3).join(', ')} +${titles.length - 3} more`;

  const planLosses = $derived(
    lossGroups((plan?.notes ?? []).flatMap((n) => n.losses.map((loss) => ({ title: n.title, loss })))),
  );
  const reportLosses = $derived(
    lossGroups((report?.losses ?? []).map(([uuid, loss]) => ({ title: titleOf(uuid), loss }))),
  );

  async function runPreview() {
    const token = ++previewToken;
    plan = null;
    previewError = null;
    previewing = true;
    try {
      const p = await restorePreview(buildRequest());
      if (token === previewToken) plan = p;
    } catch (e) {
      if (token === previewToken) previewError = friendlyBackupMessage(String(e));
    } finally {
      if (token === previewToken) previewing = false;
    }
  }

  async function apply() {
    if (applying || !plan) return;
    applying = true;
    error = null;
    try {
      report = await restoreApply(buildRequest());
      step = 'done';
    } catch (e) {
      error = friendlyBackupMessage(String(e));
    } finally {
      applying = false;
    }
  }

  // ---- navigation ---------------------------------------------------------

  function next() {
    error = null;
    if (step === 'account') {
      const latest = slugEntry?.versions[0];
      if (!slugEntry || !latest) return;
      chosenStamp = slugEntry.versions.some((v) => v.stamp === chosenStamp) ? chosenStamp : latest.stamp;
      confirmDelete = null;
      step = 'version';
    } else if (step === 'version') {
      if (!manifest) return;
      selected = new Set(manifest.notes.map((n) => n.uuid));
      search = '';
      collapsed = new Set();
      targetAccount = targets.some((a) => a.id === manifest.accountId) ? manifest.accountId : (targets[0]?.id ?? null);
      placementKind = 'original';
      existing = 'keepBoth';
      step = 'what';
    } else if (step === 'what') {
      step = 'where';
    } else if (step === 'where') {
      step = 'preview';
      void runPreview();
    }
  }

  function back() {
    error = null;
    if (step === 'version') step = 'account';
    else if (step === 'what') step = 'version';
    else if (step === 'where') step = 'what';
    else if (step === 'preview') {
      previewToken++;
      plan = null;
      previewError = null;
      previewing = false;
      step = 'where';
    }
  }

  async function removeVersion(stamp: string) {
    const slug = chosenSlug;
    if (!slug || deleting) return;
    deleting = true;
    error = null;
    try {
      await deleteBackupVersion(destinationId, slug, stamp);
      const fresh = await loadSlug(destinationId, slug);
      slugs = slugs.map((s) => (s.slug === slug ? fresh : s));
      if (chosenStamp === stamp) chosenStamp = fresh.versions[0]?.stamp ?? null;
      confirmDelete = null;
    } catch (e) {
      error = String(e);
    } finally {
      deleting = false;
    }
  }

  const primary = $derived.by(() => {
    switch (step) {
      case 'account':
        return { label: 'Next', onclick: next, disabled: loading || !slugEntry || slugEntry.versions.length === 0 };
      case 'version':
        // An empty backup (taken while the account had no notes) has nothing to pick.
        return { label: 'Next', onclick: next, disabled: !version || deleting || version.manifest.notes.length === 0 };
      case 'what':
        return { label: 'Next', onclick: next, disabled: selected.size === 0 };
      case 'where':
        // Live pass 2026-10-24: "Preview" read like it might restore. It only checks.
        return { label: 'Review…', onclick: next, disabled: !targetAccount };
      case 'preview':
        return {
          label: writes > 0 ? `Restore ${plural(writes, 'note', 'notes')}` : 'Restore',
          onclick: () => void apply(),
          disabled: previewing || !plan || writes === 0 || applying,
          busy: applying,
        };
      case 'done':
        return { label: 'Done', onclick: () => void modal?.requestClose() };
    }
  });
</script>

{#if open}
  <Modal
    bind:this={modal}
    title="Restore from backup"
    size="lg"
    locked={applying}
    {onclose}
    {primary}
    footer={step === 'account' || step === 'done' ? undefined : footer}
  >
    <div class="restore">
      {#if step === 'account'}
        <h3>Which account?</h3>
        {#if loading}
          <p class="muted">Reading the backup folder…</p>
        {:else if slugs.length === 0 && !error}
          <p class="muted">No backups in this folder yet.</p>
        {:else}
          {#if initialAccountId && !slugs.some((x) => x.versions[0]?.manifest.accountId === initialAccountId)}
            <p class="muted">No backups of {nameOf(initialAccountId)} in this folder yet. Other accounts' backups are below.</p>
          {/if}
          <div class="choices" role="radiogroup" aria-label="Account">
            {#each slugs as s (s.slug)}
              {@const latest = s.versions[0]}
              <label class="choice" class:disabled={!latest}>
                <input
                  type="radio"
                  name="restore-account"
                  checked={chosenSlug === s.slug}
                  disabled={!latest}
                  onchange={() => (chosenSlug = s.slug)}
                />
                <span class="choice-text">
                  <span class="strong">{latest ? nameOf(latest.manifest.accountId) : s.slug}</span>
                  <span class="muted small">
                    {#if latest}
                      {plural(s.versions.length, 'backup', 'backups')} · latest {when(latest.manifest.createdMs)}
                    {:else if s.error}
                      {s.error}
                    {:else}
                      No complete backups
                    {/if}
                  </span>
                </span>
              </label>
            {/each}
          </div>
        {/if}
      {:else if step === 'version' && slugEntry}
        <h3>Which backup of {slugEntry.versions[0] ? nameOf(slugEntry.versions[0].manifest.accountId) : slugEntry.slug}?</h3>
        {#if slugEntry.versions.length === 0}
          <p class="muted">No backups left for this account.</p>
        {/if}
        <div class="choices" role="radiogroup" aria-label="Backup">
          {#each slugEntry.versions as v (v.stamp)}
            <div class="version">
              <label class="choice">
                <input
                  type="radio"
                  name="restore-version"
                  checked={chosenStamp === v.stamp}
                  onchange={() => (chosenStamp = v.stamp)}
                />
                <span class="choice-text">
                  <span class="strong">{when(v.manifest.createdMs)}</span>
                  <span class="muted small">
                    {TRIGGERS[v.manifest.trigger] ?? v.manifest.trigger} · {v.manifest.notes.length === 0 ? 'Empty' : plural(v.manifest.notes.length, 'note', 'notes')}{#if partialCount(v) > 0}{' · '}{partialCount(v)} partial{/if} · {fmtBytes(v.manifest.zipBytes)}
                  </span>
                </span>
              </label>
              {#if confirmDelete === v.stamp}
                <span class="confirm">
                  <span class="small">Delete this backup for good?</span>
                  <button type="button" class="link danger" disabled={deleting} onclick={() => removeVersion(v.stamp)}>Delete backup</button>
                  <button type="button" class="link" onclick={() => (confirmDelete = null)}>Cancel</button>
                </span>
              {:else}
                <button type="button" class="link" onclick={() => (confirmDelete = v.stamp)}>Delete</button>
              {/if}
              {#if v.manifest.skipped.length > 0}
                <details class="skipped">
                  <summary>{plural(new Set(v.manifest.skipped.map(([uuid]) => uuid)).size, 'note', 'notes')} skipped</summary>
                  <ul>
                    <!-- One entry per vanished attachment, so a uuid can repeat. -->
                    {#each v.manifest.skipped as [uuid, reason], i (i)}
                      <li><code>{uuid}</code> — {reason}</li>
                    {/each}
                  </ul>
                </details>
              {/if}
            </div>
          {/each}
        </div>
        {#if version && version.manifest.notes.length === 0}
          <p class="muted">This backup is empty: the account had no notes when it was taken. Choose another backup.</p>
        {/if}
      {:else if step === 'what' && manifest}
        <h3>What to restore?</h3>
        <div class="toolbar">
          <input type="search" class="field" aria-label="Search notes" placeholder="Search notes" bind:value={search} />
          <button type="button" class="link" onclick={() => setMany(shownUuids, true)}>Select all</button>
          <button type="button" class="link" onclick={() => setMany(shownUuids, false)}>Select none</button>
        </div>
        <p class="muted small" role="status">
          {selected.size} of {manifest.notes.length} notes selected
        </p>
        <div class="tree">
          {#each rows as r (r.kind === 'folder' ? 'f:' + r.node.path : 'n:' + r.note.uuid)}
            {#if r.kind === 'folder'}
              {@const st = folderState(r.uuids)}
              <div class="row" style:padding-left="calc({r.depth} * var(--space-4))">
                <button
                  type="button"
                  class="chev"
                  aria-label="{collapsed.has(r.node.path) ? 'Expand' : 'Collapse'} {r.node.name}"
                  aria-expanded={!collapsed.has(r.node.path)}
                  disabled={!!query}
                  onclick={() => toggleCollapsed(r.node.path)}
                >{collapsed.has(r.node.path) && !query ? '▸' : '▾'}</button>
                <label class="tick folder">
                  <input
                    type="checkbox"
                    checked={st.checked}
                    disabled={r.uuids.length === 0}
                    use:indeterminate={st.indeterminate}
                    onchange={() => setMany(r.uuids, !st.checked)}
                  />
                  <span>{r.node.name}</span>
                </label>
              </div>
            {:else}
              <div class="row" style:padding-left="calc(({r.depth} + 1) * var(--space-4))">
                <label class="tick">
                  <input
                    type="checkbox"
                    checked={selected.has(r.note.uuid)}
                    onchange={() => setMany([r.note.uuid], !selected.has(r.note.uuid))}
                  />
                  <span>{r.note.title || '(untitled)'}</span>
                  {#if r.note.partial}
                    <span class="badge" title={r.note.partial}>partial</span>
                  {/if}
                </label>
              </div>
            {/if}
          {:else}
            <p class="muted small">No note matches.</p>
          {/each}
        </div>
      {:else if step === 'where' && manifest}
        <h3>Where to put it?</h3>
        <p class="muted small">Review shows what will happen. Nothing changes until you press Restore on the next screen.</p>
        {#if targets.length === 0}
          <p class="muted">No account here can take restored notes.</p>
          <button type="button" class="link" onclick={addAccount}>Add an account…</button>
        {:else}
          <label class="stack">
            <span class="strong">Restore into</span>
            <select class="field" aria-label="Restore into" value={targetAccount} onchange={(e) => (targetAccount = e.currentTarget.value)}>
              {#each targets as a (a.id)}
                <option value={a.id}>{accountDisplay(a)} · {backendLabel(a.backend_kind ?? '')}</option>
              {/each}
            </select>
          </label>
          <p class="muted small">
            Any of your accounts, not only the one backed up.
            To restore into a new one, <button type="button" class="link" onclick={addAccount}>add an account…</button>
            then open this backup again.
          </p>
        {/if}

        <fieldset class="group">
          <legend class="strong">Folders</legend>
          <label class="choice">
            <input type="radio" name="restore-placement" checked={placementKind === 'original'} onchange={() => (placementKind = 'original')} />
            <span class="choice-text"><span>Original folders</span></span>
          </label>
          <label class="choice">
            <input type="radio" name="restore-placement" checked={placementKind === 'new'} onchange={() => (placementKind = 'new')} />
            <span class="choice-text">
              <span>Under a new folder</span>
              <span class="muted small">{newFolderName}</span>
            </span>
          </label>
        </fieldset>

        {#if sameAccount}
          <fieldset class="group">
            <legend class="strong">If a note is already there</legend>
            <label class="choice">
              <input type="radio" name="restore-existing" checked={existing === 'keepBoth'} onchange={() => (existing = 'keepBoth')} />
              <span class="choice-text"><span>Keep both</span><span class="muted small">The backed-up note is added as a copy.</span></span>
            </label>
            <label class="choice">
              <input type="radio" name="restore-existing" checked={existing === 'replace'} onchange={() => (existing = 'replace')} />
              <span class="choice-text">
                <span>Replace</span>
                <span class="muted small">The note is overwritten with the backed-up text. Jodd backs up the notes it would overwrite first.</span>
              </span>
            </label>
            <label class="choice">
              <input type="radio" name="restore-existing" checked={existing === 'skip'} onchange={() => (existing = 'skip')} />
              <span class="choice-text"><span>Skip</span><span class="muted small">The note stays as it is.</span></span>
            </label>
          </fieldset>
        {/if}
      {:else if step === 'preview'}
        <h3>Before you restore</h3>
        <p class="muted small">Nothing has been changed yet.</p>
        {#if previewing}
          <p class="muted" role="status">Checking what will happen…</p>
        {:else if previewError}
          <p class="error" role="alert">{previewError}</p>
        {:else if plan}
          <ul class="counts">
            {#each counts as c (c.kind)}
              <li data-action={c.kind}><span class="strong">{c.n}</span> {c.text}</li>
            {/each}
          </ul>
          {#if writes === 0}
            <p class="muted">
              Nothing to restore: the selected notes are already in this account, or can't be restored.
              <!-- Only Skip and the same-text check can be changed by another choice; a note
                   already restored from this backup has its copy in the account already.
                   And only where that choice exists and is not already made: a restore into
                   another account offers no Keep both (Android live pass 2026-10-24). -->
              {#if sameAccount && existing !== 'keepBoth' && counts.some((c) => c.kind === 'skipExisting' || c.kind === 'skipContentMatch')}
                To add a copy of a note that is still there, go Back and choose <strong>Keep both</strong>.
              {/if}
            </p>
          {/if}
          {#if plan.foldersToCreate.length > 0}
            <h4>New folders</h4>
            <ul class="plain">
              {#each plan.foldersToCreate as f (f)}<li>{f}</li>{/each}
            </ul>
          {/if}
          {#if refused.length > 0}
            <h4>Can't be restored</h4>
            <ul class="plain">
              {#each refused as r, i (i)}<li><span class="strong">{r.title}</span> — {r.reason}</li>{/each}
            </ul>
          {/if}
          {#if planLosses.length > 0}
            <h4>What won't carry over</h4>
            {#each planLosses as g (g.heading)}
              <p class="small strong group-head">{g.heading}</p>
              <ul class="plain">
                {#each g.entries as e (e.text)}
                  <li>{e.text} <span class="muted small">({noteList(e.titles)})</span></li>
                {/each}
              </ul>
            {/each}
          {/if}
        {/if}
      {:else if step === 'done' && report}
        <h3>Restored</h3>
        <p>Created {report.created} · Replaced {report.replaced} · Skipped {report.skipped}</p>
        {#if report.refused.length > 0}
          <h4>Can't be restored</h4>
          <ul class="plain">
            {#each report.refused as [uuid, reason] (uuid)}<li><span class="strong">{titleOf(uuid)}</span> — {reason}</li>{/each}
          </ul>
        {/if}
        {#if reportLosses.length > 0}
          <h4>What didn't carry over</h4>
          {#each reportLosses as g (g.heading)}
            <p class="small strong group-head">{g.heading}</p>
            <ul class="plain">
              {#each g.entries as e (e.text)}
                <li>{e.text} <span class="muted small">({noteList(e.titles)})</span></li>
              {/each}
            </ul>
          {/each}
        {/if}
      {/if}

      {#if error}<p class="error" role="alert">{error}</p>{/if}
    </div>
  </Modal>
{/if}

{#snippet footer()}
  <button type="button" class="back-btn" disabled={applying} onclick={back}>Back</button>
{/snippet}

<style>
  .restore { display: flex; flex-direction: column; gap: var(--space-2); min-width: 0; }
  h3 { margin: 0; font-size: var(--size-md); font-weight: 600; }
  h4 { margin: var(--space-3) 0 var(--space-1); font-size: var(--size); font-weight: 600; }
  .muted { color: var(--text-muted); }
  .small { font-size: var(--size-sm); }
  .strong { font-weight: 600; }
  .error {
    background: var(--danger-wash); color: var(--danger); padding: var(--space-2);
    border-radius: var(--radius); font-size: var(--size-sm); margin: 0;
  }
  p { margin: 0; line-height: var(--leading); }

  .choices { display: flex; flex-direction: column; gap: var(--space-1); }
  .choice {
    display: flex; align-items: flex-start; gap: var(--space-2); cursor: pointer;
    padding: var(--space-1-5) var(--space-1);
  }
  .choice.disabled { opacity: 0.6; cursor: default; }
  .choice-text { display: flex; flex-direction: column; min-width: 0; }
  .version { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); border-bottom: 1px solid var(--border-subtle); }
  .version .choice { flex: 1 1 14rem; }
  .confirm { display: inline-flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
  .skipped { flex: 1 1 100%; padding: 0 var(--space-1) var(--space-2) var(--space-6); font-size: var(--size-sm); color: var(--text-muted); }
  .skipped ul { margin: var(--space-1) 0 0; padding-left: var(--space-4); }

  .toolbar { display: flex; align-items: center; gap: var(--space-3); flex-wrap: wrap; }
  .toolbar .field { flex: 1 1 10rem; min-width: 0; }
  .field {
    padding: var(--space-1-5) var(--space-2); font: inherit; font-size: var(--size-sm);
    background: var(--surface-editor); color: var(--text);
    border: 1px solid var(--border-strong); border-radius: var(--radius); box-sizing: border-box;
    max-width: 100%;
  }
  .tree {
    border: 1px solid var(--border-subtle); border-radius: var(--radius);
    padding: var(--space-1); max-height: 45vh; overflow-y: auto;
  }
  .row { display: flex; align-items: center; gap: var(--space-1); min-width: 0; }
  .tick { display: flex; align-items: center; gap: var(--space-2); padding: var(--space-1) 0; min-width: 0; cursor: pointer; }
  .tick.folder { font-weight: 600; }
  .chev {
    background: none; border: none; color: var(--text-muted); cursor: pointer;
    padding: 0 var(--space-1); font: inherit; width: var(--space-5);
  }
  .chev:disabled { opacity: 0.4; cursor: default; }
  .badge {
    font-size: var(--size-xs); color: var(--text-muted); border: 1px solid var(--border);
    border-radius: var(--radius-sm); padding: 0 var(--space-1);
  }

  .stack { display: flex; flex-direction: column; gap: var(--space-1); }
  .group { border: none; padding: 0; margin: var(--space-2) 0 0; display: flex; flex-direction: column; gap: var(--space-1); }
  .group legend { padding: 0; margin-bottom: var(--space-1); }

  .counts, .plain { margin: 0; padding-left: var(--space-5); line-height: var(--leading); }
  .plain { padding-left: var(--space-4); }
  .group-head { margin: var(--space-2) 0 0; }

  .link {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font: inherit; font-size: var(--size-sm); text-decoration: underline;
  }
  .link.danger { color: var(--danger); }
  .link:disabled { opacity: 0.5; cursor: default; }
  .back-btn {
    background: var(--surface-panel); color: var(--text); border: 1px solid var(--border);
    border-radius: var(--radius); padding: var(--space-1) var(--space-3); font: inherit;
    font-size: var(--size-sm); cursor: pointer;
  }
  .back-btn:disabled { opacity: 0.6; cursor: default; }
  :global(.modal.touch) .link, :global(.modal.touch) .back-btn, :global(.modal.touch) .field { min-height: var(--touch-target); }
</style>
