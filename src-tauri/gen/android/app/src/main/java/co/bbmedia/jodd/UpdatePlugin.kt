package co.bbmedia.jodd

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.activity.result.ActivityResult
import androidx.core.content.FileProvider
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File

@InvokeArg
class InstallApkArgs {
  lateinit var path: String
}

/**
 * In-app update, Android side (src-tauri/src/app_update/android.rs does the check,
 * download and SHA-256 verification). This class only asks for "Install
 * unknown apps" and hands a verified APK to the system Package installer.
 *
 * ACTION_INSTALL_PACKAGE, not ACTION_VIEW and not a PackageInstaller session:
 * on a Galaxy Tab S7 (2026-11-10) Google Files' own install session failed
 * silently after Play Protect allowed it, while the system Package installer
 * installed the same file. ACTION_INSTALL_PACKAGE is answered only by that
 * installer; ACTION_VIEW offered unrelated apps in a chooser.
 *
 * Like StoragePlugin, a pending requestInstallPermission never resolves if
 * Android recreates the activity while Settings is open, so the Rust side
 * holds no lock across it.
 */
@TauriPlugin
class UpdatePlugin(private val activity: Activity) : Plugin(activity) {

  private fun grantedResult(): JSObject {
    val result = JSObject()
    result.put("granted", activity.packageManager.canRequestPackageInstalls())
    return result
  }

  @Command
  fun canInstallPackages(invoke: Invoke) {
    invoke.resolve(grantedResult())
  }

  @Command
  fun requestInstallPermission(invoke: Invoke) {
    if (activity.packageManager.canRequestPackageInstalls()) {
      invoke.resolve(grantedResult())
      return
    }
    try {
      val intent = Intent(
        Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
        Uri.parse("package:" + activity.packageName)
      )
      startActivityForResult(invoke, intent, "installPermissionResult")
    } catch (ex: ActivityNotFoundException) {
      invoke.reject("Couldn't open the Install unknown apps settings")
    }
  }

  @Suppress("UNUSED_PARAMETER")
  @ActivityCallback
  fun installPermissionResult(invoke: Invoke, _result: ActivityResult) {
    // The settings page returns RESULT_CANCELED whatever the user did: re-check.
    invoke.resolve(grantedResult())
  }

  @Command
  fun installApk(invoke: Invoke) {
    val args = invoke.parseArgs(InstallApkArgs::class.java)
    val file = File(args.path).canonicalFile
    // Only a file Jodd itself downloaded into its own cache, and only an APK.
    val cache = activity.cacheDir.canonicalFile
    if (!file.path.startsWith(cache.path + File.separator) || !file.name.endsWith(".apk") || !file.isFile) {
      invoke.reject("Not a downloaded update: ${file.name}")
      return
    }
    val uri = try {
      FileProvider.getUriForFile(activity, activity.packageName + ".fileprovider", file)
    } catch (ex: IllegalArgumentException) {
      invoke.reject("Couldn't share the update with the installer")
      return
    }
    @Suppress("DEPRECATION")
    val intent = Intent(Intent.ACTION_INSTALL_PACKAGE).apply {
      setDataAndType(uri, "application/vnd.android.package-archive")
      addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }
    try {
      activity.startActivity(intent)
      invoke.resolve(JSObject())
    } catch (ex: ActivityNotFoundException) {
      invoke.reject("This device has no package installer for the update")
    }
  }
}
