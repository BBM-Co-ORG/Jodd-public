import type { SignInBlock } from './stores/ui';

/**
 * The `oauth-error` event payload, as emitted by `OauthError` in
 * src-tauri/src/lib.rs. `adminConsentUrl` is camelCase on the wire — the Rust
 * struct carries `#[serde(rename_all = "camelCase")]` and a test pins it,
 * because a rename on either side would just deserialise to `undefined` and
 * silently drop the Copy button.
 */
export interface OauthErrorPayload {
  message: string;
  adminConsentUrl: string | null;
}

/**
 * Decide where a sign-in failure is shown.
 *
 * A failure carrying an admin-consent URL has something the user can act on,
 * so it earns the panel. Everything else keeps the pre-existing route into the
 * `error` store, which is deliberately left alone here: widening the panel to
 * cover every OAuth failure would also change what an already-signed-in user
 * sees when adding a second account, and that is a separate decision.
 */
export function routeOauthError(
  payload: OauthErrorPayload,
): { panel: SignInBlock } | { errorText: string } {
  if (payload.adminConsentUrl) {
    return { panel: { message: payload.message, adminConsentUrl: payload.adminConsentUrl } };
  }
  return { errorText: payload.message };
}

const PROJECT_URL = 'https://jodd.bbmedia.co.th';

/**
 * The message a blocked user forwards to their IT administrator.
 *
 * Written to be actionable by someone who has never heard of Jodd: what is
 * being asked for, what it reaches, and the fact that the consent screen will
 * say "unverified" — an admin who meets that unwarned is an admin who says no.
 * The scopes named here must stay in step with `SCOPES` in
 * src-tauri/src/auth_ms.rs.
 */
export function adminRequestText(adminConsentUrl: string): string {
  return [
    'Hello,',
    '',
    'I would like to use Jodd with my work Microsoft account, but our organisation',
    'requires an administrator to approve the app before I can sign in.',
    '',
    'Approval link (grants consent for the organisation):',
    adminConsentUrl,
    '',
    'What it asks for: Mail.ReadWrite, offline_access and User.Read. These are',
    'delegated permissions, so the app only ever reaches the mailbox of the person',
    'signed in to it. Jodd uses them to sync Apple Notes, which Apple stores as',
    'messages in the mailbox.',
    '',
    'Two things to expect, both harmless:',
    '- Jodd is a Developer Preview, so the consent screen shows it as "unverified"',
    '  and not published by Microsoft.',
    '- After you approve, your browser will land on an error page ("this page',
    '  isn\'t working"). The approval is already recorded at that point — the',
    '  redirect afterwards has nowhere to go.',
    '',
    `More about Jodd: ${PROJECT_URL}`,
  ].join('\n');
}
