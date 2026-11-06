import type { IconName } from './components/Icon.svelte';
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
    case 'unreviewed':
      return 'list_unreviewed_notes';
  }
}

/**
 * The Smart Folder rows, in display order: the Sidebar's Views group and the
 * phone Smart folders pane both list them from here. Keyed by kind so a new
 * kind is a type error until it gets a row. The row's NAME is not here: it
 * comes from SMART_FOLDER_NAMES (viewName.ts), which the list header and the
 * empty pane read too, so a rename lands everywhere.
 */
export const SMART_FOLDER_ROWS: Record<SmartFolderKind, { icon: IconName; title?: string }> = {
  orphaned: { icon: 'search' },
  stale: { icon: 'clock' },
  extracts: { icon: 'sparkle', title: 'Notes made with AI from a source, wherever they are filed' },
  unreviewed: {
    icon: 'eye',
    title: "Notes changed by agents, Jodd's AI or another device that you have not reviewed",
  },
};

export const SMART_FOLDER_KINDS = Object.keys(SMART_FOLDER_ROWS) as SmartFolderKind[];
