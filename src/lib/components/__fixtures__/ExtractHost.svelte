<script lang="ts">
  // The Extract modal bound to extractModalOpen exactly as App.svelte does,
  // so a test can close it and open it again.
  import LessonExtractModal from '../LessonExtractModal.svelte';
  import { extractModalOpen } from '../../stores/ui';
</script>
<LessonExtractModal bind:open={$extractModalOpen} />
