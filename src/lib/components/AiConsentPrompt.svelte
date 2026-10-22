<script lang="ts">
  // One app-level prompt for a consent refusal raised where nothing inline can
  // answer it (privacy PR3, spec 2026-10-08 §4.5/§5). Mounted once in
  // App.svelte. ConfirmDialog, not confirm(): WKWebView drops native dialogs.
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { consentRequest, answerConsent, ALLOW_AI_LABEL } from '../aiConsent';
  import { accounts, accountDisplayById } from '../stores/notes';
</script>

{#if $consentRequest}
  <!-- keyed: a replaced request gets a fresh dialog (ConfirmDialog settles once) -->
  {#key $consentRequest}
    <ConfirmDialog
      title="Allow AI for this account?"
      message={`AI data access is not allowed for ${accountDisplayById($accounts, $consentRequest.accountId)}. Allowing it lets Jodd send this account's notes, titles, tags and folder names to the AI provider set for it. Data already sent cannot be recalled. Run the action again afterwards.`}
      confirmLabel={ALLOW_AI_LABEL}
      onConfirm={() => answerConsent(true)}
      onCancel={() => answerConsent(false)}
    />
  {/key}
{/if}
