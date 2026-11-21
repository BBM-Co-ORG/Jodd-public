// @vitest-environment jsdom
// Ruling 9 (source notes, fix round 1): copy notices are one line each,
// wrapped, never cut — every other toast is the one-line toast it was.
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import Toast from './Toast.svelte';
import { showToast, showToastLines, toast } from '../stores/toast';

let host: HTMLElement;
let component: ReturnType<typeof mount>;

beforeEach(() => {
  vi.useFakeTimers();
  toast.set(null);
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(Toast, { target: host });
});

afterEach(async () => {
  await unmount(component);
  host.remove();
  vi.useRealTimers();
});

it('shows three notices as three lines that wrap', () => {
  showToastLines(['Saved copy of a.example unchanged', 'Saved copy of b.example updated', "Couldn't check c.example; used the copy saved on 2026-11-19"]);
  flushSync();
  const box = host.querySelector('.toast')!;
  expect(box.classList.contains('toast-lines')).toBe(true);
  const lines = [...host.querySelectorAll('.toast-line')].map((l) => l.textContent);
  expect(lines).toEqual(['Saved copy of a.example unchanged', 'Saved copy of b.example updated', "Couldn't check c.example; used the copy saved on 2026-11-19"]);
});

it('stays longer the more lines it has', () => {
  showToastLines(['one']);
  const one = vi.getTimerCount();
  expect(one).toBe(1);
  vi.advanceTimersByTime(4_999);
  flushSync();
  expect(host.querySelectorAll('.toast-line')).toHaveLength(1);
  vi.advanceTimersByTime(1);
  flushSync();
  expect(host.querySelector('.toast')).toBeNull();

  showToastLines(['one', 'two', 'three']);
  vi.advanceTimersByTime(9_999);
  flushSync();
  expect(host.querySelectorAll('.toast-line')).toHaveLength(3);
  vi.advanceTimersByTime(1);
  flushSync();
  expect(host.querySelector('.toast')).toBeNull();
});

it('leaves the ordinary toast as it was: one line, no line list', () => {
  showToast('Copied [[a-1234abcd]]');
  flushSync();
  const box = host.querySelector('.toast')!;
  expect(box.textContent).toBe('Copied [[a-1234abcd]]');
  expect(box.classList.contains('toast-lines')).toBe(false);
  expect(host.querySelectorAll('.toast-line')).toHaveLength(0);
});
