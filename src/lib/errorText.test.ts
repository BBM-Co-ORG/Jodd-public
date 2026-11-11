// The error bar's words (errorText.ts). The inputs are the backend's real
// shapes: TransportError's Display (backend/mod.rs) and reqwest's own text.
import { describe, it, expect } from 'vitest';
import {
  errorKind, rawText, failureText, listingFailureText, isCalm, plural, placeName, DETAIL_PREFIX,
} from './errorText';

const OFFLINE = 'transient: error sending request for url (https://gmail.googleapis.com/gmail/v1/users/me/messages?q=label%3ANotes)';
const RATE = 'rate limited (retry_after=Some(30s))';
const GMAIL_403 = 'permanent: Gmail 403: {"error":{"code":403,"message":"Insufficient Permission"}}';

describe('what kind of failure it is', () => {
  it('reads offline from every shape the backend and reqwest give it', () => {
    for (const raw of [
      OFFLINE,
      'error sending request for url (https://p31-ckdatabasews.icloud.com/database/1/…)',
      'operation timed out',
      'dns error: failed to lookup address information: nodename nor servname provided',
      'tcp connect error: Connection refused (os error 61)',
      new Error('The Internet connection appears to be offline.'),
    ]) expect(errorKind(raw), String(raw)).toBe('offline');
  });

  it('a rate limit is a rate limit, even when it came as a transient', () => {
    expect(errorKind(RATE)).toBe('rate_limited');
    expect(errorKind('transient: Gmail 429 Too Many Requests')).toBe('rate_limited');
  });

  it('anything else is just a failure', () => {
    expect(errorKind(GMAIL_403)).toBe('other');
    expect(errorKind('Folder not found: Notes/Gone')).toBe('other');
    expect(errorKind(undefined)).toBe('other');
  });

  it('drops the prefixes a person has no use for', () => {
    expect(rawText(new Error('boom'))).toBe('boom');
    expect(rawText('permanent: Gmail 403')).toBe('Gmail 403');
  });
});

describe('a failed action', () => {
  it('says what did not happen, and moves machine text to a details line', () => {
    expect(failureText('delete the note', GMAIL_403)).toBe(
      `Couldn't delete the note.\n${DETAIL_PREFIX}Gmail 403: {"error":{"code":403,"message":"Insufficient Permission"}}`,
    );
  });

  it("keeps a backend reason that is already a sentence as the reason", () => {
    expect(failureText('move 3 notes', 'This folder is read-only on Outlook.'))
      .toBe("Couldn't move 3 notes. This folder is read-only on Outlook.");
  });

  it('a sentence with a URL in it is still machine text', () => {
    expect(failureText('sign in with Google', 'Could not open https://accounts.google.com/o/oauth2.'))
      .toContain(`\n${DETAIL_PREFIX}`);
  });

  it('offline and rate-limited say what to do, with no raw text', () => {
    expect(failureText('copy the note', OFFLINE))
      .toBe("Couldn't copy the note. Jodd can't reach the internet right now. Try again when you're back online.");
    expect(failureText('copy the note', RATE))
      .toBe("Couldn't copy the note. The server asked Jodd to slow down. Try again in a minute.");
  });

  it('with nothing to say about why, it says only what', () => {
    expect(failureText('save this note', '')).toBe("Couldn't save this note.");
  });
});

describe('a listing that could not finish', () => {
  it('offline is calm: the notes are on this device and will sync', () => {
    const text = listingFailureText('Gmail', OFFLINE);
    expect(text).toBe("Can't reach Gmail right now. Your notes are safe on this device and will sync when you're back online.");
    expect(isCalm(text)).toBe(true);
  });

  it('a rate limit is calm too, and starts with a capital', () => {
    const text = listingFailureText(placeName('ssh'), RATE);
    expect(text).toBe('Your SSH server asked Jodd to slow down. Syncing will resume on its own.');
    expect(isCalm(text)).toBe(true);
  });

  it('anything else is a failure that names the account', () => {
    const text = listingFailureText('Gmail (a@example.com)', GMAIL_403);
    expect(text.split('\n')[0]).toBe("Couldn't refresh Gmail (a@example.com).");
    expect(isCalm(text)).toBe(false);
  });

  it('two calm accounts stay calm; one real failure among them does not', () => {
    const icloud = listingFailureText('iCloud', OFFLINE);
    const gmail = listingFailureText('Gmail', OFFLINE);
    expect(isCalm(`${icloud}\n${gmail}`)).toBe(true);
    expect(isCalm(`${icloud}\n${listingFailureText('Gmail', GMAIL_403)}`)).toBe(false);
    expect(isCalm(null)).toBe(false);
  });
});

describe('counting', () => {
  it('says one and many, with separators', () => {
    expect(plural(1, 'note')).toBe('1 note');
    expect(plural(3, 'note')).toBe('3 notes');
    expect(plural(1204, 'duplicate')).toBe('1,204 duplicates');
  });
});
