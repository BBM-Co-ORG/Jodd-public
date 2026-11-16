<script lang="ts">
  // AiLimits has no Save of its own: Settings › AI saves it with the page's
  // one Save (saveAll over the page's forms). This is that Save, for one form.
  import AiLimits from '../../src/lib/components/AiLimits.svelte';
  import { saveAll, type SectionForm } from '../../src/lib/settings/sectionForm';
  let limits: SectionForm | undefined = $state();
  let saving = $state(false);
  const dirty = $derived(limits ? limits.isDirty() : false);
  async function save() {
    if (!limits) return;
    saving = true;
    try { await saveAll([limits]); } finally { saving = false; }
  }
</script>

<AiLimits bind:this={limits} />
<button type="button" onclick={save} disabled={!dirty || saving}>Save</button>
