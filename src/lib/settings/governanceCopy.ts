// The governance facts that must stay visible (spec 2026-11-11, "The Explicit
// Rule"). Each page renders these constants, and explicitRule.test.ts asserts
// they are on screen — never inside a closed <details> or a Learn more — so
// the page and the test share one source of the wording.

/** D9 / E14: shown under the Privacy & Data title. */
export const PER_DEVICE = 'These choices apply on this device. Your other devices keep their own.';

/** D8 / E13: always on, no setting. */
export const MASKING = 'Before any AI request, passwords, keys and personal numbers are replaced with placeholders. Always on.';
export const MASKING_KINDS = 'Personal numbers means e-mail, phone, Thai ID, card, bank account and passport numbers.';
export const MASKING_LIMITS = 'Names and addresses are not detected. Audio for speech to text is not masked.';

/** E5. */
export const ALLOW_COVERS = 'Allowing covers note text, titles, tags and folder names. Data already sent cannot be recalled.';
export const HIDDEN_FOLDERS_HELP = "Jodd's AI features and agents never see notes in hidden folders.";
export const PROVIDER_DISABLED = 'AI is Disabled for this account.';

/** E6, on the AI agents row. */
export const AGENTS_READ = 'Registered agents read every active account except hidden folders.';
export const AGENTS_OWN_RULES = 'They follow their own rules, not the allow switches below.';

/** E8. */
export const NOTES_ON_DISK_PLAIN = 'The copy is not encrypted. Only the accounts you choose are in it.';

/** E9. */
export const BACKUPS_PLAIN = 'Backups are plain files, not encrypted.';
export const BACKUPS_REMOVE_KEEPS = 'Removing a folder only makes Jodd forget it; its backups stay.';

/**
 * E10. Not "never note text": a Gmail message that fails to parse logs the
 * first 800 characters of its raw JSON (`backend/gmail/wire.rs` fetch_note),
 * which can hold the note's text. Say what is logged, as it is.
 */
export const LOGS_HOLD = 'Holds account addresses and folder names, and part of a note whose download failed.';
export const LOGS_TRIMMED = 'Trimmed past 20 MB. Logs stay on this device.';
export const LOGS_DEFAULT_ON = 'On unless you turn it off.';

/** E11. */
export const ASK_HISTORY_KEEPS = 'Keeps your questions and answers on this device. Never synced.';

/** E3. */
export const RECEIPTS_KEEP = 'Metadata only: no note text, prompts or answers are kept.';

/** E1: the enrichment switch runs AI without a click. */
export const ENRICHMENT_RUNS_ALONE = 'When on, Jodd calls AI after saving an AI result, without asking.';

/** E12. */
export const LINKS_COMMENTERS = "Commenters' names are never saved, and Jodd never signs in to read comments.";

/** E15. */
export const NOT_ON_THIS_DEVICE = 'Not on this device.';
