// The folder picker behind Add Local Folder and Add backup folder, moved out of
// AccountAddHost.svelte with its behaviour unchanged.
//
// Android: the Kotlin StoragePlugin, never the desktop dialog (which has no
// folder picker on Android) -- spec 2026-09-29-localfs-android. It needs All
// files access first, and asks for it once. Desktop: the native directory
// dialog.
import { invoke } from '@tauri-apps/api/core';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { get } from 'svelte/store';
import { isAndroid } from '../stores/platform';

/** What the caller can add around the picker. */
export type PickFolderHooks = {
  /**
   * Wraps each Android plugin call. AccountAddHost uses it to know which call
   * is pending, so it can free its button when Android orphans one.
   */
  track?: <T>(call: Promise<T>) => Promise<T>;
  /** The inline confirm (window.confirm is unreliable in WKWebView). */
  confirm?: (title: string, message: string) => Promise<boolean>;
};

type Access = { granted: boolean; supported: boolean };

/**
 * The chosen folder, or null if the user cancelled, access was refused, or
 * `isStale()` says the flow was orphaned while a plugin call was pending (a
 * stale flow must not continue).
 */
export async function pickFolder(
  title: string,
  isStale: () => boolean,
  hooks: PickFolderHooks = {},
): Promise<string | null> {
  const track = hooks.track ?? (<T>(call: Promise<T>) => call);
  const confirm = hooks.confirm ?? ((t: string, m: string) => Promise.resolve(window.confirm(`${t}\n\n${m}`)));
  let dir: string | null;
  if (get(isAndroid)) {
    let access = await track(invoke<Access>('local_folder_access'));
    if (isStale()) return null;
    if (!access.supported) {
      alert('Local Folder on Android needs Android 11 or newer.');
      return null;
    }
    if (!access.granted) {
      const go = await confirm(
        'All files access',
        'To share a folder with Syncthing, Jodd needs All files access. Android will open a settings page: turn Jodd on, then come back.',
      );
      if (!go) return null;
      access = await track(invoke<Access>('request_local_folder_access'));
      if (isStale()) return null;
      if (!access.granted) {
        alert("Without All files access, Jodd can't use a folder that other apps can see.");
        return null;
      }
    }
    dir = await track(invoke<string | null>('pick_local_folder'));
  } else {
    const picked = await openDialog({ directory: true, multiple: false, title });
    dir = typeof picked === 'string' ? picked : null;
  }
  if (isStale()) return null;
  return dir || null;
}
