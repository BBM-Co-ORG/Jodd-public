<script lang="ts">
  // Settings › Speech to text: today's SpeechToTextSettings, saved by the page
  // Save — which still asks to confirm a new host before anything is written.
  import SpeechToTextSettings from '../SpeechToTextSettings.svelte';
  import SaveError from './SaveError.svelte';
  import type { PageSaveError, SectionForm } from '../../settings/sectionForm';

  let { saveError = null }: { saveError?: PageSaveError } = $props();

  let stt: SectionForm | undefined = $state();

  /** The section-form contract (lib/settings/sectionForm.ts). */
  export function forms(): SectionForm[] {
    return stt ? [stt] : [];
  }
</script>

<section class="section">
  <p class="help">Turns video-link audio into text. Audio goes only to the host you confirm here.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      Turns the sound of a TikTok, Instagram, Facebook or X video link into
      text when the platform has no transcript.
    </p>
  </details>
  <SaveError error={saveError} index={0} />
  <SpeechToTextSettings bind:this={stt} />
</section>

<style>
  .section { margin-bottom: var(--space-4); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
</style>
