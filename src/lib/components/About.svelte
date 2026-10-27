<script lang="ts">
  import { onMount } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { aboutModalOpen, whatsNewOpen, whatsNewVersions } from '../stores/ui';
  import { whatsNewForCurrentVersion } from '../whatsNew';
  import Modal from './Modal.svelte';

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

{#if $aboutModalOpen}
  <Modal title="Jodd" size="sm" phone="page" onclose={close}>
    <div class="about-content">
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
      </div>
    </div>
  </Modal>
{/if}

<style>
  .about-content { text-align: center; }
  .about-version { color: var(--text-secondary); font-size: var(--size); margin-bottom: var(--space-2); }
  .about-desc { font-size: var(--size); color: var(--text-secondary); margin: 0 0 var(--space-2); }
  .about-disclaimer { font-size: var(--size-xs); color: var(--text-muted); margin: 0 0 var(--space-4); line-height: var(--leading); }
  .about-actions { display: flex; gap: var(--space-2); justify-content: center; flex-wrap: wrap; }
  .about-link {
    padding: var(--space-1-5) var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--surface-sidebar);
    cursor: pointer;
    font-size: var(--size);
  }
  :global(dialog.touch) .about-link {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
  }
</style>
