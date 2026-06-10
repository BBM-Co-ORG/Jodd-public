# Jodd — TODO

## 🔴 Blocking (must fix to run)
- [ ] Fix OAuth client_id not loading — `dotenv::from_path("../.env")` in lib.rs
- [ ] Fix Gmail label ID → name mapping in gmail.rs

## 🟡 Important (core functionality)
- [ ] Fix note save — replace existing message instead of appending
      (insert new → delete old, pass Gmail message ID not UUID)
- [ ] Handle note folders correctly in sidebar (nested labels)
- [ ] Show loading state while fetching notes after auth
- [ ] Error handling UI — show error to user when Gmail API fails

## 🟢 Nice to have (v0.2)
- [ ] Local SQLite cache for offline + faster load
- [ ] Note search
- [ ] Refresh notes button
- [ ] Token refresh — handle expired access token with refresh token
- [ ] Better autosave — debounce + only save when content actually changed
- [ ] Note created date vs modified date
- [ ] Android version

## 💡 Future
- [ ] Rich text toolbar (bold, italic, lists) — limited by Gmail HTML format
- [ ] Image support — not possible with Gmail backend
- [ ] Conflict resolution — if note edited on both Apple Notes and Jodd
