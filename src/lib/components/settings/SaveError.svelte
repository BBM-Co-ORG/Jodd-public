<script lang="ts">
  // A failed page Save, shown inline above the form that failed (spec §4.3:
  // "stops at the first failure, showing the error inline above that
  // section"). `index` is this form's place in the page's forms().
  import type { PageSaveError } from '../../settings/sectionForm';
  let { error, index }: { error: PageSaveError; index: number } = $props();
</script>

{#if error && error.index === index}
  <p class="save-error" role="alert">{error.message}</p>
{/if}

<style>
  .save-error {
    margin: 0 0 var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    background: var(--danger-wash);
    color: var(--danger);
    font-size: var(--size-sm);
    line-height: var(--leading);
  }
</style>
