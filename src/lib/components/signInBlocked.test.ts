import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { routeOauthError, adminRequestText } from '../signInBlocked';

describe('routing a sign-in failure to a surface', () => {
  it('sends a refusal carrying an admin-consent URL to the panel', () => {
    const out = routeOauthError({
      message: 'Sign-in was not completed.',
      adminConsentUrl: 'https://login.microsoftonline.com/organizations/adminconsent?client_id=X',
    });
    expect(out).toEqual({
      panel: {
        message: 'Sign-in was not completed.',
        adminConsentUrl:
          'https://login.microsoftonline.com/organizations/adminconsent?client_id=X',
      },
    });
  });

  it('leaves every other failure on the pre-existing error route', () => {
    const out = routeOauthError({ message: 'PKCE verifier missing', adminConsentUrl: null });
    expect(out).toEqual({ errorText: 'PKCE verifier missing' });
  });

  it('treats an absent field as no URL rather than showing an empty link', () => {
    // A Rust-side rename would deserialise to `undefined`, not `null`. The
    // panel must not open with nothing in the box.
    const out = routeOauthError({ message: 'boom' } as never);
    expect(out).toEqual({ errorText: 'boom' });
  });
});

describe('the message forwarded to an administrator', () => {
  const URL = 'https://login.microsoftonline.com/organizations/adminconsent?client_id=CID';

  it('carries the approval link verbatim', () => {
    // Forwarding a truncated or reformatted consent URL wastes the exchange
    // with IT entirely, so the link must survive intact on its own line.
    expect(adminRequestText(URL).split('\n')).toContain(URL);
  });

  it('warns that the post-approval page looks like a failure', () => {
    // Measured 2026-08-18: omitting `redirect_uri` sends the admin to a
    // registered loopback URI nothing is listening on, so a successful grant
    // ends on a browser error page. An admin who is not warned reports the
    // success as a failure.
    expect(adminRequestText(URL)).toMatch(/error page|isn't working/);
  });

  it('warns that the consent screen will say unverified', () => {
    // An admin who meets the unverified badge unwarned is an admin who
    // refuses. See HISTORY.md, 2026-08-14.
    expect(adminRequestText(URL)).toMatch(/unverified/);
  });

  it('names the exact scopes the Rust side actually requests', () => {
    // The claim made to a security-conscious admin has to be true. Read from
    // auth_ms.rs rather than duplicated, so a scope change breaks this test
    // instead of quietly making the message a lie.
    const rust = readFileSync('src-tauri/src/auth_ms.rs', 'utf8');
    const scopes = rust.match(/pub const SCOPES: &str = "([^"]+)"/)?.[1];
    expect(scopes, 'SCOPES not found — did auth_ms.rs change shape?').toBeTruthy();
    const text = adminRequestText(URL);
    for (const scope of scopes!.split(' ')) {
      expect(text, `message omits the ${scope} scope it is asking an admin to grant`).toContain(
        scope,
      );
    }
  });
});
