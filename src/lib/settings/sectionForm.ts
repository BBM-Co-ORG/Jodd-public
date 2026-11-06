// The contract a settings section implements so one Save can cover a page.
//
// `isDirty()` says whether the section holds edits that are not on disk;
// `save()` writes them and THROWS `Error(<user-readable message>)` on failure
// (a section may still show the same message itself, but never only there).
export interface SectionForm {
  isDirty(): boolean;
  save(): Promise<void>;
}

export type SaveAllResult = { ok: true } | { ok: false; index: number; message: string };

// Saves the dirty forms in order and stops at the first one that throws.
// `index` is the position in `forms`, so the caller can scroll to the section.
export async function saveAll(forms: SectionForm[]): Promise<SaveAllResult> {
  for (let index = 0; index < forms.length; index++) {
    const form = forms[index];
    if (!form.isDirty()) continue;
    try {
      await form.save();
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      return { ok: false, index, message };
    }
  }
  return { ok: true };
}

// What a Settings page shows after a failed page Save: the message, inline
// above the form at `index` in that page's `forms()`.
export type PageSaveError = { index: number; message: string } | null;
