import { mount } from 'svelte';
import Fixture from './PackageA.svelte';
import '../../src/styles/tokens.css';
// No Tauri runtime, accounts, credentials or network calls. Any unexpected IPC
// fails closed; the real context menu is used only to cancel its confirmation.
Object.defineProperty(window, '__TAURI_INTERNALS__', { value: {
  invoke: () => Promise.reject(new Error('IPC forbidden in isolated fixture')),
} });
mount(Fixture, { target: document.getElementById('app')! });
