<script lang="ts">
  // Settings › AI: the app's LLM provider, AI limits, and (desktop only) the
  // agent workspace. Moved from the old AppSettings.svelte. The provider and
  // the limits are this page's forms, saved in that order by the page Save.
  import LlmProviderSettings from '../LlmProviderSettings.svelte';
  import AiLimits from '../AiLimits.svelte';
  import AgentWorkspaceSettings from '../AgentWorkspaceSettings.svelte';
  import SaveError from './SaveError.svelte';
  import { isAndroid } from '../../stores/platform';
  import type { PageSaveError, SectionForm } from '../../settings/sectionForm';

  let { saveError = null }: { saveError?: PageSaveError } = $props();

  let llm: SectionForm | undefined = $state();
  let limits: SectionForm | undefined = $state();

  /**
   * The section-form contract: provider first, then limits (display order).
   * Nothing until both are bound, so an index is always that form's place
   * and a SaveError can never land above the wrong section.
   */
  export function forms(): SectionForm[] {
    return llm && limits ? [llm, limits] : [];
  }
</script>

<section class="section">
  <h4>LLM provider</h4>
  <p class="help">
    The provider behind Ask Jodd, and behind any account set to
    “Use app default” while the box below is ticked.
  </p>
  <SaveError error={saveError} index={0} />
  <!-- Same component as an account's page, in `app` scope. The app-level
       provider is the ONLY provider behind Ask Jodd, so it needs the
       PATH-detected preset list and the probe at least as much as an
       account does; a free-text preset field made `cluade` unfalsifiable
       until the user asked a question and got a red turn. -->
  <LlmProviderSettings bind:this={llm} scope="app" showHeading={false} />
  <SaveError error={saveError} index={1} />
  <AiLimits bind:this={limits} />
</section>

<!-- jodd-mcp, which reads what this section writes, is desktop-only. -->
{#if !$isAndroid}
  <section class="section">
    <h4>Agent workspace</h4>
    <AgentWorkspaceSettings />
  </section>
{/if}

<style>
  .section { margin-bottom: var(--space-4); }
  h4 { margin: 0 0 var(--space-1-5); font-size: var(--size-md); font-weight: 600; }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
</style>
