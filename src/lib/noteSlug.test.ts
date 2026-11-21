import { describe, it, expect } from 'vitest';
import { noteSlug, uuidShort } from './noteSlug';

const GMAIL = 'A1B2C3D4-E5F6-4A7B-8C9D-0E1F2A3B4C5D';
const ICLOUD = '1a2b3c4d-0000-4000-8000-000000000001';
const MS = '<AM0PR04MB6433D2F1C0E9A1B2@AM0PR04MB6433.eurprd04.prod.outlook.com>';
const MS_OLD = '<JH0PR04MB7174E99@JH0PR04MB7174.apcprd04.prod.outlook.com>';
const MS_NEW = '<JH0PR04MB7174A12@JH0PR04MB7174.apcprd04.prod.outlook.com>';

describe('uuidShort', () => {
  it('keeps the first 8 hex digits of a plain uuid, lowercased', () => {
    expect(uuidShort(GMAIL)).toBe('a1b2c3d4');
    expect(uuidShort(ICLOUD)).toBe('1a2b3c4d');
    expect(uuidShort('A1B2C3D4E5F64A7B8C9D0E1F2A3B4C5D')).toBe('a1b2c3d4');
  });

  it('hashes the whole id when it is not a plain uuid', () => {
    expect(uuidShort('')).toBe('811c9dc5');
    expect(uuidShort(MS_OLD)).toBe('9e66d121');
    expect(uuidShort(MS_NEW)).toBe('f6a58e22');
    expect(uuidShort('a')).toBe('e40c292c');
    expect(uuidShort('foobar')).toBe('bf9cf968');
  });

  it('gives two notes on one Exchange server different ids', () => {
    expect(uuidShort(MS_OLD)).not.toBe(uuidShort(MS_NEW));
  });

  it('equals the Rust db::uuid_short (values hard-coded from Rust)', () => {
    expect(uuidShort(GMAIL)).toBe('a1b2c3d4');
    expect(uuidShort(ICLOUD)).toBe('1a2b3c4d');
    expect(uuidShort(MS)).toBe('f3ceba56');
  });
});

describe('noteSlug', () => {
  it('ends in the short id', () => {
    expect(noteSlug('Plan B', GMAIL)).toBe('plan-b-a1b2c3d4');
    expect(noteSlug('Target', MS)).toBe('target-f3ceba56');
  });

  it('is the bare short id when the title slugifies to nothing', () => {
    expect(noteSlug('   ', ICLOUD)).toBe('1a2b3c4d');
  });
});
