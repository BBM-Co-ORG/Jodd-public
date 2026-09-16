package co.bbmedia.jodd

import android.os.Bundle
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
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
}
