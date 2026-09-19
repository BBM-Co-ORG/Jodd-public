// jsdom has no showModal/close implementation. This stub only exposes open
// state; it does NOT emulate inertness, focus traversal or keyboard activation.
// Use tests/browser/package-a.html to measure those in a real browser.
Object.defineProperties(HTMLDialogElement.prototype, {
  showModal: { configurable: true, value(this: HTMLDialogElement) { this.open = true; } },
  close: { configurable: true, value(this: HTMLDialogElement) { this.open = false; } },
});
