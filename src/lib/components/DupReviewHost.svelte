<script lang="ts">
  // The extra-Gmail-copies review, mounted once for the whole app (App.svelte)
  // so it outlives the Sidebar that opened it: turning a tablet to landscape
  // closes the portrait drawer, and that drawer's Sidebar, under an open
  // review (stores/ui.ts dupReviewAccount).
  import { get } from 'svelte/store';
  import DupReviewModal from './DupReviewModal.svelte';
  import { dupReviewAccount, dupCleanupResult } from '../stores/ui';
  import { refreshNotes } from '../stores/notes';
  import { plural } from '../errorText';

  function onTrashed(accountId: string, count: number) {
    dupCleanupResult.update((r) => ({
      ...r,
      [accountId]: count === 0 ? 'No copies moved' : `Moved ${plural(count, 'extra copy', 'extra copies')} to Trash`,
    }));
    // A fresh list_notes so dup_stats recomputes from Gmail itself: the
    // backend no longer decrements the count optimistically, which flickered
    // against Gmail's eventual consistency. The Sidebar's reactive block runs
    // refreshDupStats when $notes updates.
    void Promise.resolve(get(refreshNotes)());
    setTimeout(() => dupCleanupResult.update((r) => ({ ...r, [accountId]: '' })), 4000);
  }
</script>

{#if $dupReviewAccount}
  {@const accountId = $dupReviewAccount}
  {#key accountId}
    <DupReviewModal
      {accountId}
      onClose={() => dupReviewAccount.set(null)}
      onTrashed={(count) => onTrashed(accountId, count)}
    />
  {/key}
{/if}
