package co.bbmedia.jodd

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/**
 * Local Folder vaults on Android (docs/superpowers/specs/2026-09-29-localfs-android-design.md).
 * `tauri-plugin-dialog` has no folder picker on Android, and a folder other
 * apps can see needs MANAGE_EXTERNAL_STORAGE ("All files access", API 30+).
 * Activity results go through the plugin API. An ordinary configuration change
 * is covered by the manifest's configChanges, but if Android destroys and
 * recreates the activity while Settings or the picker is open, Tauri 2.11.5
 * never resolves the pending invoke (its launchers stay registered on the first
 * activity). The frontend therefore keeps a single-flow guard and, on
 * `visible`, arms a 2 s timer that frees the button only if the same plugin
 * call is still pending (`hidden` cancels it); the abandoned flow is not
 * resumed.
 */
@TauriPlugin
class StoragePlugin(private val activity: Activity) : Plugin(activity) {

  private fun accessResult(): JSObject {
    val supported = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R
    val result = JSObject()
    result.put("supported", supported)
    // isExternalStorageManager() does not exist below API 30.
    result.put("granted", supported && Environment.isExternalStorageManager())
    return result
  }

  @Command
  fun hasAllFilesAccess(invoke: Invoke) {
    invoke.resolve(accessResult())
  }

  @Command
  fun requestAllFilesAccess(invoke: Invoke) {
    val now = accessResult()
    if (!now.getBoolean("supported") || now.getBoolean("granted")) {
      invoke.resolve(now)
      return
    }
    try {
      val intent = Intent(
        Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
        Uri.parse("package:" + activity.packageName)
      )
      startActivityForResult(invoke, intent, "accessSettingsResult")
    } catch (ex: ActivityNotFoundException) {
      // Some OEM builds lack the per-app page; fall back to the general list.
      try {
        startActivityForResult(invoke, Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION), "accessSettingsResult")
      } catch (ex2: ActivityNotFoundException) {
        invoke.reject("Couldn't open the All files access settings")
      }
    }
  }

  @Suppress("UNUSED_PARAMETER")
  @ActivityCallback
  fun accessSettingsResult(invoke: Invoke, _result: ActivityResult) {
    // The settings page returns RESULT_CANCELED whatever the user did: re-check.
    invoke.resolve(accessResult())
  }

  @Command
  fun pickFolder(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
    startActivityForResult(invoke, intent, "pickFolderResult")
  }

  @ActivityCallback
  fun pickFolderResult(invoke: Invoke, result: ActivityResult) {
    val out = JSObject()
    if (result.resultCode == Activity.RESULT_OK && result.data?.data != null) {
      out.put("uri", result.data!!.data.toString())
    } else {
      out.put("uri", null as String?)
    }
    invoke.resolve(out)
  }
}
