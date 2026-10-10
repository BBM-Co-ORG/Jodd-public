import { getVersion } from '@tauri-apps/api/app';
import changelog from './generated/changelog.json';

// What's New / release-notes wiring. Entry shape matches the generated
// changelog.json (see scripts/changelog-parse.mjs).
export type WhatsNewEntry = { version: string; date: string | null; sections: Record<string, string[]> };

const LAST_SEEN_KEY = 'jodd:lastSeenVersion';

// Compare two dotted version strings numerically. >0 if a is newer than b.
function cmpVersion(a: string, b: string): number {
  const pa = a.split('.').map((n) => parseInt(n, 10) || 0);
  const pb = b.split('.').map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

// `as unknown as` rather than a direct cast, and not out of laziness: TypeScript
// infers the imported JSON as a union of one literal type per entry, normalising
// each member's `sections` to carry `Added?: undefined` for keys the *other*
// entries have. Those optional-undefined properties are not comparable to
// `Record<string, string[]>`, so the direct cast fails the moment a release adds
// a section combination no earlier entry used — an `### Fixed`-only entry was
// enough to break it. The shape is guaranteed by scripts/changelog-parse.mjs,
// not by the inferred literal type, so asserting through `unknown` is the honest
// expression of where the contract actually lives.
function releasedEntries(): WhatsNewEntry[] {
  return (changelog as unknown as WhatsNewEntry[]).filter((e) => e.version !== 'Unreleased');
}

// Auto-shown once per version bump on launch. Compares the running app
// version to the last value recorded in localStorage; surfaces every
// changelog entry strictly newer than it (so a user who jumped several
// versions sees them all), then records the current version so it doesn't
// fire again until the next upgrade. Returns [] when there's nothing new.
export async function whatsNewForLaunch(): Promise<WhatsNewEntry[]> {
  const current = await getVersion();
  const lastSeen = localStorage.getItem(LAST_SEEN_KEY);
  if (current === lastSeen) return [];
  const all = releasedEntries();
  const versions = lastSeen
    ? all.filter((e) => cmpVersion(e.version, lastSeen) > 0)
    : all.filter((e) => e.version === current);
  localStorage.setItem(LAST_SEEN_KEY, current);
  return versions;
}

// Manually requested via About → "What's New". Independent of the
// launch/"seen" bookkeeping above — always shows the running version's own
// release notes, regardless of whether the auto-popup already fired.
export async function whatsNewForCurrentVersion(): Promise<WhatsNewEntry[]> {
  const current = await getVersion();
  return releasedEntries().filter((e) => e.version === current);
}
