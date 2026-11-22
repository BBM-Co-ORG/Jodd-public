<script lang="ts">
  // Organize, mounted once for the whole app (App.svelte) so it outlives
  // whatever opened it: the portrait tablet drawer and the phone's AI sheet
  // both close on the same tap (stores/ui.ts organizeAccount).
  import CurateReview from './CurateReview.svelte';
  import { organizeAccount } from '../stores/ui';
</script>

{#if $organizeAccount}
  <!-- Keyed: Organize for another account is a fresh screen, not this one's
       proposals with a new id. -->
  {#key $organizeAccount}
    <CurateReview accountId={$organizeAccount} onClose={() => organizeAccount.set(null)} />
  {/key}
{/if}
