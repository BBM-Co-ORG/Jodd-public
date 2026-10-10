package co.bbmedia.jodd

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // Share to Jodd: BEFORE super, because the deep-link plugin reads
    // `activity.intent` when it loads (cold start). See shareAsCapture.
    // Not on a recreation or a relaunch from Recents: both re-deliver the
    // ORIGINAL share, which would queue it again. Left as SEND, the deep-link
    // plugin ignores it.
    val fromHistory = intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0
    if (savedInstanceState == null && !fromHistory) shareAsCapture(intent)?.let { intent = it }
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    // Back at the root of the webview's history must BACKGROUND Jodd, never
    // finish the activity. Finishing it destroys tao's only window, which makes
    // tauri-runtime-wry end the event loop and tao call `std::process::exit` —
    // with the sync worker and any URL ingest still running on tokio threads,
    // which die with the process. `exit()` also runs libhwui's static
    // destructors while the UI and render threads still draw, hence the
    // `FORTIFY: pthread_mutex_lock called on a destroyed mutex` line (measured
    // 2026-09-16 on an SM-S711B: the address is in libhwui.so's .bss) — a
    // symptom of the exit, not its cause. See docs/GOTCHAS.md #31.
    //
    // Ordering is the mechanism: wry's own callback is registered later (in
    // `setWebView`), so it runs first and pops webview history — the phone
    // pane stack note → list → folders in src/lib/stores/phoneNav.ts. Only when
    // the webview cannot go back does it disable itself and re-dispatch, which
    // reaches this callback instead of the default `finish()`.
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        moveTaskToBack(true)
      }
    })
  }

  // Warm share: singleTask delivers it here, never as a second activity.
  override fun onNewIntent(intent: Intent) {
    val delivered = shareAsCapture(intent) ?: intent
    setIntent(delivered)
    super.onNewIntent(delivered)
  }

  // Share to Jodd (spec docs/superpowers/specs/2026-10-06-share-to-jodd-design.md).
  // The share sheet sends ACTION_SEND; the deep-link plugin forwards only VIEW
  // intents whose URL matches tauri.conf.json's `mobile` list. So a share is
  // REWRITTEN into `jodd://capture?text=…&title=…` and arrives in Rust through
  // the same `on_open_url` / `get_current()` path as on desktop — no new
  // JNI or reflective entry point for R8 to strip (docs/GOTCHAS.md #31).
  // Kotlin stays dumb on purpose: the link inside EXTRA_TEXT is found by
  // Rust (`capture::links`), and every cap and check lives there too.
  private fun shareAsCapture(intent: Intent?): Intent? {
    if (intent?.action != Intent.ACTION_SEND) return null
    val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
    val subject = intent.getStringExtra(Intent.EXTRA_SUBJECT)
    val uri = Uri.Builder().scheme("jodd").authority("capture")
    if (!text.isNullOrBlank()) uri.appendQueryParameter("text", text)
    if (!subject.isNullOrBlank()) uri.appendQueryParameter("title", subject)
    return Intent(Intent.ACTION_VIEW, uri.build())
  }
}
