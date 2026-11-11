<script lang="ts">
  // Settings › Privacy & Data (spec 2026-11-11): one home for data and AI
  // governance — where data goes, what each account's AI may see, the AI
  // limits, and what is kept on this device. The AI page keeps the *how*
  // (provider, AI host, connecting agents); this page holds the *what*.
  //
  // The Explicit Rule: nothing about governance is behind Learn more, inside
  // a collapsed section, or left off a platform's screen. The wording lives
  // in settings/governanceCopy.ts so explicitRule.test.ts can check it.
  //
  // Two groups, so it is plain which is which (live check 2026-11-11):
  // "Set here" holds what this page changes — the AI switches and hidden
  // folders apply at once, the limits are its one form (page Save) — and
  // "Set on other pages" is the read-only overview whose rows open the page
  // that owns each setting. No setting has two homes. `leave` moves to
  // another Settings page through the dirty guard.
  import AiLimits from '../AiLimits.svelte';
  import SaveError from './SaveError.svelte';
  import WhereDataGoes from './data/WhereDataGoes.svelte';
  import AiAccess from './data/AiAccess.svelte';
  import Records from './data/Records.svelte';
  import { PER_DEVICE, MASKING, MASKING_KINDS, MASKING_LIMITS } from '../../settings/governanceCopy';
  import type { PageSaveError, SectionForm } from '../../settings/sectionForm';

  let {
    saveError = null,
    leave,
  }: { saveError?: PageSaveError; leave: (to: string) => Promise<boolean> } = $props();

  let limits: SectionForm | undefined = $state();

  /** The section-form contract (lib/settings/sectionForm.ts): the limits only. */
  export function forms(): SectionForm[] {
    return limits ? [limits] : [];
  }
</script>

<p class="help per-device">{PER_DEVICE}</p>

<h3 class="group">Set here</h3>

<section class="section">
  <h4>AI access by account</h4>
  <AiAccess {leave} />
  <p class="help">{MASKING}</p>
  <p class="help">{MASKING_KINDS}</p>
  <p class="help">{MASKING_LIMITS}</p>
</section>

<section class="section">
  <h4>Limits</h4>
  <SaveError error={saveError} index={0} />
  <AiLimits bind:this={limits} />
</section>

<section class="section">
  <h4>Records on this device</h4>
  <Records />
</section>

<h3 class="group">Set on other pages</h3>

<section class="section">
  <h4>Where your data goes</h4>
  <p class="help">Read only. Each row opens the page where it is set.</p>
  <WhereDataGoes {leave} />
</section>

<style>
  .section { margin-bottom: var(--space-6); display: flex; flex-direction: column; gap: var(--space-2); }
  h4 { margin: 0; font-size: var(--size-md); font-weight: var(--weight-semibold); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0; line-height: var(--leading); }
  .per-device { margin-bottom: var(--space-4); color: var(--text-secondary); }
  /* DESIGN.md's Section Label: 700, 11px, 1.35, 0.04em, uppercase. */
  .group {
    margin: 0 0 var(--space-3);
    padding-top: var(--space-2);
    border-top: 1px solid var(--border);
    font-size: var(--size-xs);
    font-weight: var(--weight-bold);
    line-height: var(--leading-tight);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
</style>
