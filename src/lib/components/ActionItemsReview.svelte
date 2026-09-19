<script lang="ts">
  import type { ActionItemsPreview } from '../meetingActions';
  let { preview, busy, onapply, ondiscard }: {
    preview: ActionItemsPreview; busy: boolean; onapply: () => void; ondiscard: () => void;
  } = $props();
  // Evidence navigation stays inside the preview (including a collapsed section).
  function followEvidence(event: MouseEvent) {
    const anchor = (event.target as Element).closest('a');
    if (!anchor) return;
    event.preventDefault();
    const href = anchor.getAttribute('href');
    if (href?.startsWith('#meeting-')) {
      const container = event.currentTarget as HTMLElement;
      const passage = Array.from(container.querySelectorAll('[id]')).find(el => el.id === href.slice(1));
      passage?.scrollIntoView({ block: 'nearest' });
      if (passage instanceof HTMLElement) { passage.tabIndex = -1; passage.focus(); }
    }
  }
</script>

<section aria-label="Review meeting actions">
  <h3>Review draft</h3>
  <p><strong>{preview.title}</strong></p>
  <p>Nothing has been saved. Check full evidence passages, owners and dates. Missing information stays unspecified.</p>
  {#if preview.before_html !== null}
    <p>Append-only diff: existing content stays unchanged; the addition below is appended only after you confirm. A changed or removed target requires a new preview.</p>
    <details><summary>Unchanged existing content</summary><div class="content">{@html preview.before_html}</div></details>
    <h4>Addition</h4>
  {:else}
    <p>A separate draft note will be created. Your source stays unchanged.</p>
  {/if}
  <!-- Both HTML fields are sanitized by Rust. All links are intercepted. -->
  <!-- Native anchors emit click on Enter; the handler delegates navigation. -->
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
  <div class="content" role="document" onclick={followEvidence}>
    {@html preview.body_html}
  </div>
  <div class="review-actions">
    <button disabled={busy} onclick={ondiscard}>Back to source</button>
    <button disabled={busy} onclick={onapply}>{busy ? 'Saving locally…' : preview.before_html !== null ? 'Confirm append' : 'Save separate draft'}</button>
  </div>
</section>

<style>
  section { color: var(--text); }
  p { color: var(--text-secondary); }
  .content { border: 1px solid var(--border); border-radius: var(--radius); padding: 12px; overflow-wrap: anywhere; }
  .content :global(pre) { white-space: pre-wrap; }
  .content :global(a) { color: var(--accent-action); }
  .review-actions { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 12px; }
  button { color: var(--text); background: var(--surface-panel); border: 1px solid var(--border); padding: 8px; border-radius: var(--radius-sm); cursor: pointer; }
</style>
