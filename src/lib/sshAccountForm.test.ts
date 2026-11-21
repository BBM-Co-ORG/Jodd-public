import { describe, it, expect } from 'vitest';
import { validateTarget, addArgs, childPath, parentPath, parseAddress } from './sshAccountForm';
import { backendLabel } from './stores/notes';

describe('ssh account form', () => {
  it('mirrors the Rust target rules', () => {
    expect(validateTarget('me@box')).toBeNull();
    expect(validateTarget('box')).toBeNull();
    expect(validateTarget('')).toMatch(/empty/);
    expect(validateTarget('-oProxyCommand=x')).toMatch(/-/);
    expect(validateTarget('a b')).toMatch(/spaces/);
  });

  it('sends trimmed args and null for a blank name', () => {
    expect(addArgs({ target: ' me@box ', root: ' /srv/notes ', name: '  ', create: true })).toEqual({
      target: 'me@box', root: '/srv/notes', name: null, create: true,
    });
    expect(addArgs({ target: 'b', root: '/r', name: ' Work ', create: false }).name).toBe('Work');
  });

  it('walks remote paths', () => {
    expect(childPath('/home/me', 'notes')).toBe('/home/me/notes');
    expect(childPath('/', 'srv')).toBe('/srv');
    expect(parentPath('/home/me')).toBe('/home');
    expect(parentPath('/home')).toBe('/');
    expect(parentPath('/')).toBe('/');
  });

  it('labels the backend', () => {
    expect(backendLabel('ssh')).toBe('SSH');
  });
});

describe('parseAddress', () => {
  it('splits user@host:port', () => {
    expect(parseAddress('me@box.example.com:2222')).toEqual({ host: 'box.example.com', port: 2222, user: 'me' });
  });
  it('splits host:port with no user', () => {
    expect(parseAddress('box.example.com:2222')).toEqual({ host: 'box.example.com', port: 2222, user: null });
  });
  it('splits user@host with no port', () => {
    expect(parseAddress('me@box.example.com')).toEqual({ host: 'box.example.com', port: null, user: 'me' });
  });
  it('accepts a bare host', () => {
    expect(parseAddress('box.example.com')).toEqual({ host: 'box.example.com', port: null, user: null });
  });
});
