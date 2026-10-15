<script lang="ts">
  import { onMount } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { aboutModalOpen, whatsNewOpen, whatsNewVersions } from '../stores/ui';
  import { whatsNewForCurrentVersion } from '../whatsNew';

  const REPO_URL = 'https://github.com/BBM-Co-ORG/Jodd-public';
  let version = '';

  onMount(async () => {
    try {
      version = await getVersion();
    } catch (e) {
      console.error('getVersion failed', e);
      version = 'unknown';
    }
  });

  function close() {
    aboutModalOpen.set(false);
  }
  async function showWhatsNew() {
    aboutModalOpen.set(false);
    try {
      whatsNewVersions.set(await whatsNewForCurrentVersion());
    } catch (e) {
      console.error('whats-new lookup failed', e);
      whatsNewVersions.set([]);
    }
    whatsNewOpen.set(true);
  }
</script>

<svelte:window onkeydown={(e) => { if ($aboutModalOpen && e.key === 'Escape') close(); }} />

{#if $aboutModalOpen}
  <div class="about-overlay" role="presentation" onclick={close}>
    <div
      class="about-modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="about-title"
      onclick={(e) => e.stopPropagation()}
    >
      <h2 id="about-title">Jodd</h2>
      <div class="about-version">Version {version}</div>
      <p class="about-desc">Local-first notes across devices — a BBMedia Developer Preview.</p>
      <p class="about-disclaimer">
        Jodd is an independent, unofficial project. Not affiliated with,
        endorsed by, or sponsored by Apple Inc. or Google LLC.
      </p>
      <div class="about-actions">
        <button class="about-link" onclick={showWhatsNew}>What's New</button>
        <button class="about-link" onclick={() => openUrl(REPO_URL)}>Project page</button>
        <button class="about-link" onclick={() => openUrl('https://jodd.bbmedia.co.th/privacy.html')}>Privacy</button>
        <button class="about-link" onclick={() => openUrl('https://jodd.bbmedia.co.th/terms.html')}>Terms</button>
        <button class="about-close" onclick={close}>Close</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .about-overlay {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .about-modal {
    background: var(--surface-editor);
    border-radius: 10px;
    padding: 20px 24px;
    width: min(380px, 90vw);
    box-shadow: var(--shadow-modal);
    text-align: center;
    /* Short today, but it is the same unscrollable shape as App Settings was
       — and this one carries a changelog, which only ever grows. */
    max-height: 85vh;
    overflow-y: auto;
  }
  h2 { margin: 0 0 4px; font-size: var(--size-xl); }
  .about-version { color: var(--text-secondary); font-size: var(--size); margin-bottom: 8px; }
  .about-desc { font-size: var(--size); color: var(--text-secondary); margin: 0 0 8px; }
  .about-disclaimer { font-size: var(--size-xs); color: var(--text-muted); margin: 0 0 16px; line-height: var(--leading); }
  .about-actions { display: flex; gap: 8px; justify-content: center; flex-wrap: wrap; }
  .about-link, .about-close {
    padding: 6px 14px;
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    background: var(--surface-sidebar);
    cursor: pointer;
    font-size: var(--size);
  }
</style>
