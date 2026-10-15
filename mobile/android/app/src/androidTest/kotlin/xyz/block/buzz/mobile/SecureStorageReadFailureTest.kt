package xyz.block.buzz.mobile

import android.content.Context
import android.content.SharedPreferences
import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.it_nomads.fluttersecurestorage.FlutterSecureStorageConfig
import io.flutter.FlutterInjector
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.dart.DartExecutor
import io.flutter.plugin.common.MethodChannel
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/** Uses the real pinned plugin, Android Keystore and production Dart services. */
@RunWith(AndroidJUnit4::class)
class SecureStorageReadFailureTest {
    @Test
    fun decryptFailurePreservesDataAndNeverGenerates() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        // Never install/run the corruption probe using a person's app identity.
        assumeTrue("Requires the disposable storage-probe build", context.packageName.endsWith(".identitysafety"))
        val done = CountDownLatch(1)
        var failure: Throwable? = null
        var passed = false
        var engine: FlutterEngine? = null
        var preferences: SharedPreferences? = null
        var corruptedKey: String? = null
        var original: String? = null
        var damagedSnapshot: Map<String, *>? = null
        try {
            instrumentation.runOnMainSync {
                val loader = FlutterInjector.instance().flutterLoader()
                loader.startInitialization(context)
                loader.ensureInitializationComplete(context, null)
                val running = FlutterEngine(context)
                engine = running
                MethodChannel(running.dartExecutor.binaryMessenger, "buzz/storage_failure_probe")
                    .setMethodCallHandler { call, reply ->
                        try {
                            when (call.method) {
                                "isolated" -> reply.success(true)
                                "corrupt" -> {
                                    val options = call.argument<Map<String, Any>>("options")!!
                                    val config = FlutterSecureStorageConfig(options)
                                    val prefs = context.getSharedPreferences(config.effectiveDataPrefsName, Context.MODE_PRIVATE)
                                    val key = config.sharedPreferencesKeyPrefix + "_" + call.argument<String>("key")!!
                                    preferences = prefs
                                    corruptedKey = key
                                    original = prefs.getString(key, null)
                                    assertTrue(original != null, "Probe must corrupt an existing encrypted record")
                                    // Keep framing/Base64 valid, but invalidate the ciphertext authentication tag.
                                    val damaged = Base64.decode(original, Base64.DEFAULT)
                                    assertTrue(damaged.isNotEmpty())
                                    damaged[damaged.lastIndex] = (damaged.last().toInt() xor 1).toByte()
                                    assertTrue(prefs.edit().putString(key, Base64.encodeToString(damaged, Base64.DEFAULT)).commit())
                                    damagedSnapshot = prefs.all.toMap()
                                    reply.success(null)
                                }
                                "assertPreserved" -> {
                                    assertEquals(damagedSnapshot, preferences!!.all)
                                    reply.success(null)
                                }
                                "restore" -> {
                                    assertTrue(preferences!!.edit().putString(corruptedKey, original).commit())
                                    reply.success(null)
                                }
                                "done" -> {
                                    passed = call.argument<Boolean>("passed") == true
                                    reply.success(null)
                                    done.countDown()
                                }
                                else -> reply.notImplemented()
                            }
                        } catch (error: Throwable) {
                            failure = error
                            reply.error("probe_failure", error.javaClass.simpleName, null)
                            done.countDown()
                        }
                    }
                running.dartExecutor.executeDartEntrypoint(
                    DartExecutor.DartEntrypoint(loader.findAppBundlePath(), "main"),
                )
            }
            assertTrue(done.await(45, TimeUnit.SECONDS), "Dart probe did not complete")
            failure?.let { throw it }
            assertTrue(passed, "Production Dart resolver did not fail closed")
        } finally {
            if (original != null) preferences?.edit()?.putString(corruptedKey, original)?.commit()
            instrumentation.runOnMainSync { engine?.destroy() }
        }
    }
}
