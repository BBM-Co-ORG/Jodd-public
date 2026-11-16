<script lang="ts">
  // One app-level prompt for a consent refusal raised where nothing inline can
  // answer it (privacy PR3, spec 2026-10-08 §4.5/§5). Mounted once in
  // App.svelte. ConfirmDialog, not confirm(): WKWebView drops native dialogs.
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { consentRequest, answerConsent, allowAiLabel } from '../aiConsent';
  import { accounts, accountDisplayById } from '../stores/notes';
</script>

{#if $consentRequest}
  <!-- keyed: a replaced request gets a fresh dialog (ConfirmDialog settles once) -->
  {#key $consentRequest}
    <ConfirmDialog
      title={`${allowAiLabel(accountDisplayById($accounts, $consentRequest.accountId))}?`}
      message={`AI data access is not allowed for ${accountDisplayById($accounts, $consentRequest.accountId)}. Allowing it lets Jodd send this account's notes, titles, tags and folder names to the AI provider set for it. Data already sent cannot be recalled. Run the action again afterwards.`}
      confirmLabel={allowAiLabel(accountDisplayById($accounts, $consentRequest.accountId))}
      onConfirm={() => answerConsent(true)}
      onCancel={() => answerConsent(false)}
    />
  {/key}
{/if}
