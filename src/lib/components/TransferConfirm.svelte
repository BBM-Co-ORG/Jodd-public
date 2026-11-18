<script lang="ts">
  // The one confirm a Copy to / Move to asks (spec
  // 2026-11-13-cross-account-move-copy §2.3, §4.4): what each note loses in
  // the target, which notes are skipped and why, and for a move out of
  // Outlook, that the original goes for good. Mounted once in App;
  // controllers/noteTransfer.ts sets the question and awaits the answer.
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { transferQuestion } from '../controllers/noteTransfer';
</script>

{#if $transferQuestion}
  {@const q = $transferQuestion}
  <ConfirmDialog
    title={q.title}
    message={q.message}
    confirmLabel={q.confirmLabel}
    onConfirm={() => q.answer(true)}
    onCancel={() => q.answer(false)}
  />
{/if}
