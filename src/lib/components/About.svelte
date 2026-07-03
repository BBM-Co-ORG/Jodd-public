<script lang="ts">
  import { onMount } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { aboutModalOpen, whatsNewOpen } from '../stores/ui';

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
  function showWhatsNew() {
    aboutModalOpen.set(false);
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
      <p class="about-desc">Apple Notes, anywhere — by BBM Media.</p>
      <div class="about-actions">
        <button class="about-link" onclick={showWhatsNew}>What's New</button>
        <button class="about-link" onclick={() => openUrl(REPO_URL)}>Project page</button>
        <button class="about-close" onclick={close}>Close</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .about-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .about-modal {
    background: #fdfcf7;
    border-radius: 10px;
    padding: 20px 24px;
    width: min(380px, 90vw);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
    text-align: center;
  }
  h2 { margin: 0 0 4px; font-size: 20px; }
  .about-version { color: #5a5a52; font-size: 13px; margin-bottom: 8px; }
  .about-desc { font-size: 13px; color: #444; margin: 0 0 16px; }
  .about-actions { display: flex; gap: 8px; justify-content: center; flex-wrap: wrap; }
  .about-link, .about-close {
    padding: 6px 14px;
    border: 1px solid #cfcabb;
    border-radius: 6px;
    background: #efece2;
    cursor: pointer;
    font-size: 13px;
  }
</style>
