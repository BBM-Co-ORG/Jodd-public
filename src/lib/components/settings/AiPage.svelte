<script lang="ts">
  // Settings › AI: the app's LLM provider and (desktop only) connecting agents
  // — the *how*. What AI may see, its limits and what is kept moved to
  // Settings › Privacy & Data (spec 2026-11-11). The provider is this page's form.
  import LlmProviderSettings from '../LlmProviderSettings.svelte';
  import AgentWorkspaceSettings from '../AgentWorkspaceSettings.svelte';
  import AgentFilesSettings from '../AgentFilesSettings.svelte';
  import AgentSkillsSettings from '../AgentSkillsSettings.svelte';
  import SaveError from './SaveError.svelte';
  import { isAndroid } from '../../stores/platform';
  import type { PageSaveError, SectionForm } from '../../settings/sectionForm';

  let { saveError = null }: { saveError?: PageSaveError } = $props();

  let llm: SectionForm | undefined = $state();

  /** The section-form contract (lib/settings/sectionForm.ts): the provider. */
  export function forms(): SectionForm[] {
    return llm ? [llm] : [];
  }
</script>

<section class="section">
  <h4>AI provider</h4>
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
  <p class="help">What AI may see, its limits and what is kept are in Settings › Privacy &amp; Data.</p>
</section>

<!-- jodd-mcp, which reads what this section writes, is desktop-only. -->
{#if !$isAndroid}
  <section class="section">
    <h4>Agent workspace</h4>
    <AgentWorkspaceSettings />
  </section>
  <section class="section">
    <h4>Notes on disk for Claude Code</h4>
    <AgentFilesSettings />
  </section>
  <section class="section">
    <h4>Skills for Claude Code</h4>
    <AgentSkillsSettings />
  </section>
{/if}

<style>
  .section { margin-bottom: var(--space-4); }
  h4 { margin: 0 0 var(--space-1-5); font-size: var(--size-md); font-weight: var(--weight-semibold); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
</style>
