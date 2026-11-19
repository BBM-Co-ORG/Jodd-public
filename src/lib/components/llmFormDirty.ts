/**
 * Does the LLM provider form hold edits that Save has not written yet?
 *
 * "Test connection" runs the SAVED configuration — it has to, since the probe
 * happens in Rust against what is on disk. Pick a provider from the dropdown,
 * press Test without pressing Save, and the honest result is a failure about
 * the *previous* configuration. The form meanwhile shows the newly picked
 * preset with a ✓ next to it, because the ✓ means "this binary is on PATH",
 * which is true and unrelated. So the screen says ready and the probe says
 * "provider not configured", and both are correct.
 *
 * Comparing the form against what was last saved is what lets the UI say which
 * of those two things the user is actually looking at.
 */

/** A stable string for everything Save persists. */
export function formFingerprint(payload: unknown, applyToAccounts: boolean): string {
  return JSON.stringify({ payload, applyToAccounts });
}

/**
 * `saved` is null until the initial load lands — before that there is nothing
 * to compare against and nothing has been edited, so the answer is "no".
 */
export function hasUnsavedEdits(
  saved: string | null,
  current: string,
  typedApiKey: string,
): boolean {
  if (typedApiKey !== '') return true;
  if (saved === null) return false;
  return saved !== current;
}
