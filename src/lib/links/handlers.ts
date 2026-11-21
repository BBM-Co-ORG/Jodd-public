// What opening a link DOES, and which buttons its chip shows. Pure: the editor
// supplies a `LinkContext`; nothing here touches Svelte, stores or IPC.
//
// Which gesture may open what:
//   web, os (mailto/tel) — chip or modifier-click (what `opener:default` allows)
//   os (named app)       — chip only: a deliberate click on a labelled button
//   note, anchor         — chip or modifier-click
//   inert                — never; the chip offers Copy so the URI can be read

import { namedApp, schemeOf, type LinkTarget } from './target';

export interface LinkContext {
  openUrl(url: string): Promise<void>;
  openNote(t: LinkTarget): Promise<void>;
  scrollToAnchor(id: string): void;
  copy(text: string): void;
  editLink(a: HTMLAnchorElement): void;
  removeLink(a: HTMLAnchorElement): void;
}

export interface LinkAction {
  id: string;
  label: string;
  run(ctx: LinkContext): void;
}

export type OpenVia = 'chip' | 'modifier';

/** A named-app scheme: opens from the chip only. */
function isNamedApp(t: LinkTarget): boolean {
  const s = schemeOf(t.uri);
  return t.kind === 'os' && s !== null && namedApp(s) !== undefined;
}

/** Fire an async open; a rejection is logged, never left unhandled. */
function fire(p: Promise<void>, what: string): void {
  Promise.resolve(p).catch((e) => console.warn(`[links] ${what} failed`, e));
}

/** Open a target by the given gesture. Returns true only when it acted. */
export function primaryOpen(t: LinkTarget, ctx: LinkContext, via: OpenVia): boolean {
  switch (t.kind) {
    case 'note':
      fire(ctx.openNote(t), 'open note');
      return true;
    case 'anchor':
      ctx.scrollToAnchor(t.uri.replace(/^#/, ''));
      return true;
    case 'web':
      fire(ctx.openUrl(t.uri), 'open url');
      return true;
    case 'os':
      if (via === 'modifier' && isNamedApp(t)) return false;
      fire(ctx.openUrl(t.uri), 'open url');
      return true;
    case 'inert':
      return false;
  }
}

/** The chip's buttons for a target. */
export function chipActions(t: LinkTarget): LinkAction[] {
  const open = (label: string): LinkAction => ({
    id: 'open',
    label,
    run: (ctx) => void primaryOpen(t, ctx, 'chip'),
  });
  const copy = (label: string, text = t.uri): LinkAction => ({
    id: 'copy',
    label,
    run: (ctx) => ctx.copy(text),
  });

  switch (t.kind) {
    case 'note':
      return [open('Open'), copy('Copy text', `[[${t.key ?? t.label}]]`)];
    case 'anchor':
      return [open('Go to')];
    case 'web': {
      const acts = [open('Open in browser'), copy('Copy link')];
      const a = t.anchor;
      if (a) {
        acts.push(
          { id: 'edit', label: 'Edit link', run: (ctx) => ctx.editLink(a) },
          { id: 'remove', label: 'Remove link', run: (ctx) => ctx.removeLink(a) },
        );
      }
      return acts;
    }
    case 'os': {
      const s = schemeOf(t.uri);
      const app = s ? namedApp(s)?.app : undefined;
      return [open(app ? `Open in ${app}` : 'Open'), copy('Copy')];
    }
    case 'inert':
      return [copy('Copy')];
  }
}
