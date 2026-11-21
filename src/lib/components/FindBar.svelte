<script lang="ts">
  // The editor's Find / Replace bar: markup and styles only. Every decision —
  // matching, painting, what Replace consumes — stays in NoteEditor.svelte and
  // ../editor/findReplace.ts; this reports input and clicks back through the
  // callbacks below. Split out of NoteEditor.svelte 2026-11-16.
  import Icon from './Icon.svelte';

  interface Props {
    query: string;
    replaceQuery: string;
    showReplace: boolean;
    inputEl?: HTMLInputElement;
    matchCount: number;
    currentIdx: number;
    /** A regex that does not compile; empty when the pattern is fine. */
    error: string;
    matchCase: boolean;
    regex: boolean;
    /** Android: every control gets the 48px touch target. */
    touch: boolean;
    /** The editor stopped marking matches in this note: painting them made
     *  a frame too slow (../editor/frameGuard.ts). Count and ‹ › still work. */
    paused: boolean;
    onfindinput: () => void;
    onreplaceinput: () => void;
    /** Keys in either input: Enter steps, Escape leaves. */
    onfieldkey: (e: KeyboardEvent) => void;
    /** Keys anywhere in the bar: Escape from a button closes it too. */
    onbarkey: (e: KeyboardEvent) => void;
    ontoggle: (option: 'case' | 'regex') => void;
    onprev: () => void;
    onnext: () => void;
    onreplace: () => void;
    onreplaceall: () => void;
    onclose: () => void;
  }

  let {
    query = $bindable(),
    replaceQuery = $bindable(),
    showReplace = $bindable(),
    inputEl = $bindable(),
    matchCount,
    currentIdx,
    error,
    matchCase,
    regex,
    touch,
    paused,
    onfindinput,
    onreplaceinput,
    onfieldkey,
    onbarkey,
    ontoggle,
    onprev,
    onnext,
    onreplace,
    onreplaceall,
    onclose,
  }: Props = $props();
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="find-bar" class:touch onkeydown={onbarkey}>
  <div class="find-row">
    <span class="find-icon"><Icon name="search" size={12} /></span>
    <input
      bind:this={inputEl}
      bind:value={query}
      oninput={onfindinput}
      onkeydown={onfieldkey}
      placeholder="Find in note"
      aria-label="Find in note"
      class="find-input"
    />
    <button class="find-opt" class:on={matchCase} aria-label="Match case" aria-pressed={matchCase} title="Match case" onclick={() => ontoggle('case')}>Aa</button>
    <button class="find-opt mono" class:on={regex} aria-label="Regular expression" aria-pressed={regex} title="Regular expression (^ $ per line, $1 in Replace)" onclick={() => ontoggle('regex')}>.*</button>
    <span class="find-count" class:error={!!error} title={error} role="status" aria-live="polite">
      {#if error}Invalid pattern{:else if query && matchCount > 0}{currentIdx + 1} / {matchCount}{:else if query}No match{/if}
    </span>
    <button class="find-btn" onclick={onprev} disabled={!matchCount} aria-label="Previous match" title="Previous match (Shift+Enter)"><Icon name="chevron-left" size={14} /></button>
    <button class="find-btn" onclick={onnext} disabled={!matchCount} aria-label="Next match" title="Next match (Enter)"><Icon name="chevron-right" size={14} /></button>
    <label class="find-toggle" title="Show replace">
      <input type="checkbox" bind:checked={showReplace} /> Replace
    </label>
    <button class="find-close" onclick={onclose} aria-label="Close find" title="Close (Esc)"><Icon name="close" size={12} /></button>
  </div>
  {#if showReplace}
    <div class="find-row">
      <span style="width: 12px"></span>
      <input
        bind:value={replaceQuery}
        oninput={onreplaceinput}
        placeholder="Replace with"
        aria-label="Replace with"
        onkeydown={onfieldkey}
        class="find-input"
      />
      <button class="find-btn-text" onclick={onreplace} disabled={!matchCount}>Replace</button>
      <button class="find-btn-text" onclick={onreplaceall} disabled={!matchCount}>Replace all</button>
    </div>
  {/if}
  {#if paused}
    <p class="find-paused" role="status">Highlighting is off for this note — it is too long to mark every match. ‹ and › still move between them.</p>
  {/if}
</div>

<style>
  .find-bar {
    /* The find row's own container: at the smallest desktop window (800px,
       default panes) the editor is ~350px and the one-row bar was 481px, its
       count, ‹ › and Close off-screen (final re-critique). Not on
       .editor-pane: a size container contains layout, which would make it
       the containing block for the pane's position: fixed panels. */
    container-type: inline-size;
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
  .find-icon {
    display: inline-flex;
    flex: none;
    color: var(--text-muted);
  }
  .find-input {
    flex: 1;
    background: var(--surface-panel);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    padding: 4px 8px;
    font-size: var(--size-sm);
    line-height: var(--leading);
    font-family: inherit;
    outline: none;
  }
  /* Blue Means Focus: the shared ring, not a stronger grey border. */
  .find-input:focus { border-color: var(--focus); box-shadow: var(--ring-focus); }
  .find-paused {
    margin: var(--space-1) 0 0;
    font-size: var(--size-xs);
    color: var(--text-muted);
  }
  .find-count {
    font-size: var(--size-xs);
    color: var(--text-muted);
    min-width: 56px;
    text-align: right;
  }
  .find-btn {
    width: var(--hit-target-desktop);
    height: var(--hit-target-desktop);
    background: var(--surface-panel);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
  }
  .find-btn:hover:not(:disabled) { background: var(--surface-list); }
  .find-opt {
    height: var(--hit-target-desktop);
    min-width: var(--hit-target-desktop);
    padding: 0 6px;
    background: var(--surface-panel);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
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
  .find-btn:disabled { opacity: 0.6; cursor: not-allowed; }
  .find-btn-text {
    min-height: var(--hit-target-desktop);
    white-space: nowrap;
    padding: 3px 10px;
    background: var(--surface-panel);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    font-size: var(--size-xs);
    color: var(--text);
    cursor: pointer;
    font-family: inherit;
  }
  .find-btn-text:hover:not(:disabled) { background: var(--surface-list); }
  .find-btn-text:disabled { opacity: 0.6; cursor: not-allowed; }
  .find-toggle {
    /* The label is the hit area for its checkbox: 28px, not the 14px of
       its text line (DESIGN.md desktop minimum). */
    min-height: var(--hit-target-desktop);
    font-size: var(--size-xs);
    color: var(--text-secondary);
    display: flex;
    align-items: center;
    gap: 3px;
    cursor: pointer;
    user-select: none;
  }
  /* A native checkbox paints in the system blue, which DESIGN.md keeps for
     the focus ring alone (Blue Means Focus): the brand's Amber Ink instead. */
  .find-toggle input {
    accent-color: var(--accent-action);
  }
  .find-close {
    min-width: var(--hit-target-desktop);
    min-height: var(--hit-target-desktop);
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

  /* Android: every control is a 48px touch target (the desktop sizes are
     untouched); NoteEditor's toolbar and link prompt carry the same rule. */
  .find-bar.touch button,
  .find-bar.touch .find-toggle {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
  }
  .find-bar.touch .find-input {
    min-height: var(--touch-target);
  }
  /* The find bar's controls are fixed 22px boxes on desktop. */
  .find-bar.touch .find-btn,
  .find-bar.touch .find-opt {
    width: auto;
    height: auto;
  }
  /* A phone is too narrow for the input and seven 48px controls in one row:
     the row wraps, the input keeps a usable width on the first line. */
  .find-bar.touch .find-row {
    flex-wrap: wrap;
  }
  .find-bar.touch .find-input {
    flex: 1 1 12em;
  }
  /* A narrow editor wraps the find row the same way, on every platform. */
  @container (max-width: 560px) {
    .find-row {
      flex-wrap: wrap;
    }
    .find-input {
      flex: 1 1 12em;
    }
  }
</style>
