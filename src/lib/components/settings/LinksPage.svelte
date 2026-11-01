<script lang="ts">
  // Settings › Links: what URL ingest may read beyond the link itself
  // (spec 2026-10-28-follow-links-and-discussions-design.md).
  import YoutubeKeySettings from '../YoutubeKeySettings.svelte';
  import AutoFollowSettings from '../AutoFollowSettings.svelte';
  import SaveError from './SaveError.svelte';
  import type { PageSaveError, SectionForm } from '../../settings/sectionForm';

  let { saveError = null }: { saveError?: PageSaveError } = $props();

  let youtube: SectionForm | undefined = $state();
  let autoFollow: SectionForm | undefined = $state();

  /** The section-form contract (lib/settings/sectionForm.ts). Order = SaveError index. */
  export function forms(): SectionForm[] {
    return [autoFollow, youtube].filter((f): f is SectionForm => !!f);
  }
</script>

<section class="section">
  <p class="help">Ingest reads comments where a site shows them to anyone.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      That covers Bluesky, Mastodon, Hacker News, forums and blogs. Only the author's own comments add links to
      follow, and commenters' names are never saved in your notes.
    </p>
    <p>
      Facebook, Instagram, TikTok, X and Reddit show comments only to signed-in users, and Jodd never uses your
      accounts. For those, paste what you found in the comments into the box under the link.
    </p>
    <p>
      YouTube comments need a free YouTube Data API key: in Google Cloud Console, enable "YouTube Data API v3" and
      create an API key. Reading comments uses about one unit of its 10 000 free daily units per video.
    </p>
  </details>
  <SaveError error={saveError} index={0} />
  <AutoFollowSettings bind:this={autoFollow} />
  <SaveError error={saveError} index={1} />
  <YoutubeKeySettings bind:this={youtube} />
</section>

<style>
  .section { margin-bottom: var(--space-4); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
</style>
