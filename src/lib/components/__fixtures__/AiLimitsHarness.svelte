<script lang="ts">
  // Mirrors what the page-level Save will do: derive "any section dirty" from
  // the sections' isDirty(). Proves isDirty() is reactive, not just correct.
  import AiLimits from '../AiLimits.svelte';
  let section: { isDirty(): boolean; save(): Promise<void> } | undefined = $state();
  const anyDirty = $derived(section ? [section].some((f) => f.isDirty()) : false);
  export function save() { return section!.save(); }
</script>
<AiLimits bind:this={section} />
<output data-testid="any-dirty">{anyDirty ? 'dirty' : 'clean'}</output>
