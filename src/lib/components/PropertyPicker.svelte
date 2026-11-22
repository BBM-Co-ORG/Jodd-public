<script lang="ts">
  // The property pickers (spec 2026-11-19 §7): the account's keys at the
  // start of a block line, a key's values after `Key: `, and dates after
  // `Due: ` / `📅 ` / `due: ` or from toolbar Due date. HashtagPicker's
  // pattern: it only presents — what to list, where, which row is active and
  // what a pick writes are NoteEditor's (../editor/propertyTrigger.ts,
  // ../editor/propertyEdits.ts) — and the body keeps focus while it is open,
  // so a row's pointerdown and mousedown are cancelled and the pick runs on
  // click.
  //
  // The date mode adds a native date field. It does take focus; NoteEditor
  // keeps the caret as a line and column (gotcha #34) and puts it back
  // before the pick is written.

  import type { PickerItem } from '../editor/propertyTrigger';

  let {
    items,
    activeIndex,
    position,
    onPick,
    dateInput = false,
    dateValue = '',
    touch = false,
    id = 'property-picker',
    label = 'Suggestions',
    onFocusLeave,
    onEscape,
  }: {
    items: PickerItem[];
    activeIndex: number;
    /** Viewport coordinates, as HashtagPicker's. */
    position: { left: number; top?: number; bottom?: number; maxHeight?: number };
    onPick: (value: string) => void;
    /** Show a native date field under the rows. */
    dateInput?: boolean;
    /** The field's starting value (`YYYY-MM-DD`). */
    dateValue?: string;
    touch?: boolean;
    /** The listbox id; row i is `${id}-${i}` (aria-activedescendant from the body). */
    id?: string;
    label?: string;
    /** Focus left the picker (from its date field) for `to`, outside it. */
    onFocusLeave?: (to: EventTarget | null) => void;
    /** Esc in the date field. */
    onEscape?: () => void;
  } = $props();

  let root: HTMLDivElement | undefined = $state();

  function onFocusOut(e: FocusEvent): void {
    const to = e.relatedTarget;
    if (!(to instanceof Node && root?.contains(to))) onFocusLeave?.(to);
  }

  // The field writes into the body only a whole date, and only when the user
  // commits it. Chromium (and Android's WebView) and WebKit fire `change` on
  // every segment edit, so typing a year digit by digit passes through
  // `0002-11-25`, `0020-11-25`, …, and a half-typed day through `…-02` — each
  // a well-formed value that would be written, synced to Apple Notes, and
  // close the picker. So: a `change` the keyboard made is held until Enter;
  // a `change` with no typing behind it (the calendar popup, the phone's date
  // dialog) is the commit. Either way the value must be a real date from
  // 1900 on. Leaving the field after typing without Enter writes nothing.
  let typedInDate = false;

  function onDateKey(e: KeyboardEvent): void {
    if (e.key === 'Escape') {
      e.preventDefault();
      onEscape?.();
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      const v = (e.currentTarget as HTMLInputElement).value;
      if (isCommittableDate(v)) onPick(v);
      return;
    }
    typedInDate = true;
  }

  const style = $derived(
    [
      `left:${position.left}px`,
      position.top !== undefined ? `top:${position.top}px` : '',
      position.bottom !== undefined ? `bottom:${position.bottom}px` : '',
      position.maxHeight !== undefined ? `max-height:${position.maxHeight}px` : '',
    ]
      .filter(Boolean)
      .join('; '),
  );

  function keepFocus(e: Event): void {
    e.preventDefault();
  }

  function onDate(e: Event): void {
    if (typedInDate) return; // a segment edit, not a commit: Enter writes it
    const v = (e.currentTarget as HTMLInputElement).value;
    if (isCommittableDate(v)) onPick(v);
  }
</script>

<script lang="ts" module>
  /** A whole `YYYY-MM-DD` that is a real calendar day, year 1900 or later. */
  export function isCommittableDate(v: string): boolean {
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(v);
    if (!m) return false;
    const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
    if (y < 1900) return false;
    const t = new Date(Date.UTC(y, mo - 1, d));
    return t.getUTCFullYear() === y && t.getUTCMonth() === mo - 1 && t.getUTCDate() === d;
  }
</script>

{#if items.length || dateInput}
  <div class="property-picker" class:touch data-property-picker data-testid="property-picker" {style} bind:this={root} onfocusout={onFocusOut}>
    {#if items.length}
      <div role="listbox" aria-label={label} {id}>
        {#each items as item, i (item.value)}
          <button
            class="opt"
            class:active={i === activeIndex}
            type="button"
            role="option"
            id={`${id}-${i}`}
            aria-selected={i === activeIndex}
            tabindex="-1"
            data-value={item.value}
            onpointerdown={keepFocus}
            onmousedown={keepFocus}
            onclick={() => onPick(item.value)}
          ><span class="name">{item.label}</span>{#if item.detail}<span class="detail">{item.detail}</span>{/if}</button>
        {/each}
      </div>
    {/if}
    {#if dateInput}
      <label class="date">
        <span class="date-label">Pick a date</span>
        <input type="date" value={dateValue} onchange={onDate} onkeydown={onDateKey} data-testid="property-date-input" />
      </label>
    {/if}
  </div>
{/if}

<style>
  .property-picker {
    position: fixed;
    z-index: var(--z-popover);
    min-width: 180px;
    max-width: min(340px, calc(100vw - 2 * var(--space-2)));
    max-height: 280px;
    overflow-y: auto;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-menu);
    padding: var(--space-1);
  }
  .opt {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    min-height: var(--hit-target-desktop);
    border: none;
    background: none;
    text-align: left;
    padding: var(--space-1-5) var(--space-2);
    border-radius: var(--radius);
    cursor: pointer;
    font-family: inherit;
    font-size: var(--size);
    color: var(--text);
    line-height: var(--leading);
  }
  .name {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .detail {
    margin-left: auto;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .touch .opt {
    min-height: var(--touch-target);
  }
  .opt:hover,
  .opt.active {
    background: var(--accent-tint);
  }
  .date {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1-5) var(--space-2);
    color: var(--text-muted);
    font-size: var(--size);
  }
  .date input {
    font-family: inherit;
    font-size: var(--size);
    color: var(--text);
    background: var(--surface-editor);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: var(--space-1) var(--space-1-5);
  }
</style>
