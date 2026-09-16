// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { matchTrigger, isTriggerKeystroke, lineBeforeCaret, ensureCheckboxesNotEditable } from './markdownTriggers';

function inputEvent(inputType: string, data: string | null): Event {
  const e = new Event('input');
  Object.defineProperty(e, 'inputType', { value: inputType });
  Object.defineProperty(e, 'data', { value: data });
  return e;
}

describe('matchTrigger', () => {
  it.each([
    ['# ', 'h1', 2],
    ['## ', 'h2', 3],
    ['### ', 'h3', 4],
    ['> ', 'blockquote', 2],
    ['- ', 'ul', 2],
    ['* ', 'ul', 2],
    ['1. ', 'ol', 3],
    ['# ', 'h1', 2],
    ['- ', 'ul', 2],
  ])('%j → %s (deletes %i chars)', (line, kind, length) => {
    expect(matchTrigger(line)).toEqual({ kind, length });
  });

  it.each(['#', '#  ', 'a# ', '#### ', '2. ', '- x', ''])('%j is not a trigger', (line) => {
    expect(matchTrigger(line)).toBeNull();
  });
});

describe('isTriggerKeystroke', () => {
  it('accepts a typed space (and the nbsp WebKit may type)', () => {
    expect(isTriggerKeystroke(inputEvent('insertText', ' '))).toBe(true);
    expect(isTriggerKeystroke(inputEvent('insertText', ' '))).toBe(true);
  });

  it('rejects a paste — measured in WebKit as insertText carrying the whole string', () => {
    expect(isTriggerKeystroke(inputEvent('insertText', '> '))).toBe(false);
    expect(isTriggerKeystroke(inputEvent('insertFromPaste', null))).toBe(false);
  });

  it('rejects undo and redo, which restore a trigger line without anyone typing it', () => {
    expect(isTriggerKeystroke(inputEvent('historyUndo', null))).toBe(false);
    expect(isTriggerKeystroke(inputEvent('historyRedo', null))).toBe(false);
  });

  it('rejects a non-space keystroke and a call with no event at all', () => {
    expect(isTriggerKeystroke(inputEvent('insertText', 'a'))).toBe(false);
    expect(isTriggerKeystroke(undefined)).toBe(false);
    expect(isTriggerKeystroke(new Event('input'))).toBe(false);
  });
});

describe('lineBeforeCaret', () => {
  it('reads only the current line of a block that holds several <br>-separated lines', () => {
    const div = document.createElement('div');
    div.innerHTML = 'first line<br>#&nbsp;';
    const text = div.lastChild as Text;
    expect(lineBeforeCaret(text, text.length)).toBe('#\u00A0');
  });

  it('includes inline siblings before the caret but stops at a block boundary', () => {
    const div = document.createElement('div');
    div.innerHTML = '<p>other</p><b>-</b> ';
    const text = div.lastChild as Text;
    expect(lineBeforeCaret(text, text.length)).toBe('- ');
  });
});

describe('ensureCheckboxesNotEditable', () => {
  it('restores contenteditable="false" on checkboxes WebKit inserted without it, and leaves the rest alone', () => {
    const root = document.createElement('div');
    root.innerHTML =
      '<div><input type="checkbox" contenteditable="false">&nbsp;kept</div>' +
      '<div><input type="checkbox">&nbsp;from insertHTML</div>' +
      '<div><input type="checkbox" checked="">&nbsp;from redo</div>' +
      '<div><input type="text">&nbsp;not a checkbox</div>';
    ensureCheckboxesNotEditable(root);
    const boxes = Array.from(root.querySelectorAll('input[type=checkbox]'));
    expect(boxes.map((b) => b.getAttribute('contenteditable'))).toEqual(['false', 'false', 'false']);
    expect(root.querySelector('input[type=text]')!.hasAttribute('contenteditable')).toBe(false);
    expect(root.querySelectorAll('input[type=checkbox]')[2].hasAttribute('checked')).toBe(true);
  });
});
