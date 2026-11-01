<script lang="ts">
  // Settings › Share to Jodd, per platform. Actions only: nothing to save.
  // Moved verbatim from the old AppSettings.svelte.
  import { invoke } from '@tauri-apps/api/core';
  import { isAndroid, isMacos } from '../../stores/platform';
  import { BOOKMARKLET, openCommand } from '../../capture';
  import { copyText } from '../../clipboard';

  // Which snippet was just copied, for the button's label.
  let copiedShare = $state<'' | 'bookmarklet' | 'command'>('');
  // The snippets are not on screen, so a failed copy cannot be done by hand —
  // it has to say so instead of leaving the button looking unclicked.
  let copyShareFailed = $state(false);
  let shortcutMsg = $state('');
  async function addShareShortcut() {
    shortcutMsg = '';
    try {
      await invoke('install_share_shortcut');
      shortcutMsg = 'Shortcuts opened — click "Add Shortcut". "Share to Jodd" then appears in the Share menu.';
    } catch (e) {
      shortcutMsg = String(e);
    }
  }
  async function copyShare(kind: 'bookmarklet' | 'command') {
    copyShareFailed = false;
    if (await copyText(kind === 'bookmarklet' ? BOOKMARKLET : openCommand(!$isMacos))) {
      copiedShare = kind;
      setTimeout(() => { if (copiedShare === kind) copiedShare = ''; }, 2000);
    } else {
      copiedShare = '';
      copyShareFailed = true;
    }
  }
</script>

<section class="section">
  {#if $isAndroid}
    <p class="help">Use Share in any app and pick Jodd. You choose what happens before anything is saved.</p>
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>Works from a browser, YouTube, another notes app and more.</p>
    </details>
  {:else}
    {#if $isMacos}
      <p class="help">Add "Share to Jodd" to the Share menu of Safari, Chrome, Finder and other apps.</p>
      <details class="learn-more">
        <summary>Learn more</summary>
        <p>Shortcuts asks before adding it.</p>
      </details>
      <div class="actions">
        <button class="link-btn" onclick={addShareShortcut}>Add to Share menu</button>
      </div>
      {#if shortcutMsg}<p class="help">{shortcutMsg}</p>{/if}
    {/if}
    <p class="help">Send a page or selected text to Jodd from any browser with a bookmark.</p>
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>
        Add a bookmark and paste this as its address. Clicking it opens Jodd
        with the page's link, title and selection; nothing is saved until
        you choose.
      </p>
    </details>
    <div class="actions">
      <button class="link-btn" onclick={() => copyShare('bookmarklet')}>
        {copiedShare === 'bookmarklet' ? 'Copied' : 'Copy bookmarklet'}
      </button>
      <button class="link-btn" onclick={() => copyShare('command')}>
        {copiedShare === 'command' ? 'Copied' : $isMacos ? 'Copy command for Raycast / Shortcuts' : 'Copy command for PowerToys'}
      </button>
    </div>
    {#if copyShareFailed}
      <p class="help copy-failed" role="alert">Could not copy to the clipboard. Try again.</p>
    {/if}
    <p class="help">Any launcher can share by opening a <code>jodd://capture</code> link.</p>
    {#if !$isMacos}
      <!-- Changes what the user does with the copied command, so it stays
           visible; why is under Learn more. -->
      <p class="help">Run the copied command in Command Prompt (<code>cmd</code>), not PowerShell.</p>
    {/if}
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>
        The link takes <code>url</code>, <code>text</code> and <code>title</code> parameters.
        In the copied command, replace the sample text with your own,
        URL-encoded (a space is <code>%20</code>).
        {#if !$isMacos}
          In PowerShell, <code>start</code> is a different command.
        {/if}
      </p>
    </details>
  {/if}
</section>

<style>
  .section { margin-bottom: var(--space-4); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--space-3); margin: 0 0 var(--space-3); }
  .copy-failed { color: var(--danger); }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font-size: var(--size-sm); text-decoration: underline;
  }
</style>
