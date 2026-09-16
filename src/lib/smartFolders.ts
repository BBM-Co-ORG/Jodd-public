import type { SmartFolderKind } from './stores/notes';

/**
 * The read command behind each Smart Folder. App.svelte invokes the result
 * dynamically, which `lib.rs`'s `ipc_contract::dynamic_invocations_are_the_known_ones_only`
 * cannot check statically — add any new command to its `known_dynamic_commands`.
 */
export function smartFolderCommand(kind: SmartFolderKind): string {
  switch (kind) {
    case 'orphaned':
      return 'list_orphaned_notes';
    case 'stale':
      return 'list_stale_notes';
    case 'extracts':
      return 'list_extract_notes';
  }
}
