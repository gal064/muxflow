package dev.muxflow.ssh

import android.content.Context
import android.os.Handler
import android.os.Looper
import expo.modules.kotlin.Promise
import expo.modules.kotlin.exception.CodedException
import expo.modules.kotlin.exception.Exceptions
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import expo.modules.kotlin.records.Field
import expo.modules.kotlin.records.Record
import java.util.concurrent.ConcurrentHashMap

/**
 * `muxflow-ssh`: a deliberately dumb SSH transport for the Muxflow phone app (design doc §6).
 *
 * It opens exec channels to `muxflow-host bridge --stdio` and moves bytes; it knows nothing about
 * the protocol those bytes carry. Every event reaches JavaScript through the single `onSshEvent`
 * event, discriminated by `type`, which is what `src/ssh/MuxflowSsh.ts` fans out into the typed
 * union in §6.1.
 *
 * Several `connectionId`s may address the same `user@host:port`; they share one authenticated
 * [SshTransport] and get one exec channel each, so the client's second "bulk" bridge costs no extra
 * login. Channels close independently; the transport goes away with the last of them.
 *
 * It also lends JavaScript a clock. React Native freezes every JS timer while the host activity is
 * paused (`JavaTimerManager.onHostPause`), so a reconnect backoff scheduled with `setTimeout` from
 * the background never fires; `scheduleWake` runs the same delay on an Android [Handler], which
 * keeps going for as long as the process — kept alive by [ConnectionService] — does, and answers
 * with an `onWake` event carrying the caller's token.
 *
 * The third event, `onDisconnectRequested`, is the notification's Disconnect action. It is sent
 * before any channel is closed and whether or not one is open: between reconnect attempts there is
 * no channel to report `closedByClient`, and without the event JavaScript would keep re-dialling
 * on its native timer with the service — and the notification — already gone.
 */
class MuxflowSshModule : Module() {
  private companion object {
    const val EVENT_NAME = "onSshEvent"
    const val WAKE_EVENT_NAME = "onWake"
    const val DISCONNECT_EVENT_NAME = "onDisconnectRequested"

    /** Exit status of a shell that could not find the command — §12 maps this to "helper missing". */
    const val EXIT_COMMAND_NOT_FOUND = 127

    /**
     * Close reasons after which §7.2 tells the client not to retry. Once the last connection ends
     * this way there is nothing left to keep the process alive for, so the service stops. A
     * `networkLost` or `exited` close is left alone: the client is about to reconnect, and
     * restarting a foreground service from the background is not always permitted.
     */
    val NON_RETRYABLE_REASONS =
      setOf(
        CloseReason.CLOSED_BY_CLIENT,
        CloseReason.AUTH_FAILED,
        CloseReason.HOST_KEY_MISMATCH,
        CloseReason.HOST_KEY_NOT_TRUSTED,
      )
  }

  /** Guards [transports] and [channels] together so a new channel cannot race a transport teardown. */
  private val registryLock = Any()
  private val transports = HashMap<String, SshTransport>()
  private val channels = ConcurrentHashMap<String, SshChannel>()

  /**
   * Pending wakes by token. The main looper is used only as a clock: the runnable does nothing but
   * hand the token back to JavaScript, so it never keeps the UI thread busy.
   */
  private val wakeHandler = Handler(Looper.getMainLooper())
  private val wakes = HashMap<String, Runnable>()

  @Volatile private var serviceTitle: String? = null
  @Volatile private var serviceBody: String = ""
  @Volatile private var serviceRunning = false

  private val context: Context
    get() = appContext.reactContext ?: throw Exceptions.ReactContextLost()

  override fun definition() = ModuleDefinition {
    Name("MuxflowSsh")

    Events(EVENT_NAME, WAKE_EVENT_NAME, DISCONNECT_EVENT_NAME)

    OnCreate {
      SshSecurity.ensureBouncyCastle()
      ConnectionService.onDisconnectRequested = { disconnectFromNotification() }
      ConnectionService.onStopped = { serviceRunning = false }
    }

    AsyncFunction("generateKeyPair") {
      mapOf("publicKeyOpenSsh" to translatingKeyStoreErrors { SshKeyStore.generate(context) })
    }

    AsyncFunction("getPublicKey") {
      translatingKeyStoreErrors { SshKeyStore.publicKeyOpenSsh(context) }
    }

    AsyncFunction("deleteKeyPair") { translatingKeyStoreErrors { SshKeyStore.delete(context) } }

    AsyncFunction("connect") {
      connectionId: String,
      target: SshTarget,
      command: String,
      trustedHostKeyFingerprint: String? ->
      val keyPair = translatingKeyStoreErrors { SshKeyStore.keyPairOrNull(context) }
      synchronized(registryLock) {
        if (channels.containsKey(connectionId)) {
          throw ConnectionAlreadyOpenException(connectionId)
        }
        val key = "${target.user}@${target.host}:${target.port}"
        val existing = transports[key]
        val transport =
          existing ?: SshTransport(key, target.host, target.port, target.user, keyPair, ::dispatch)
        transports[key] = transport
        try {
          channels[connectionId] =
            transport.open(connectionId, command, trustedHostKeyFingerprint, keyPair, ::onChannelTerminated)
        } catch (t: Throwable) {
          // A transport this call created has no channel to release it later, so it would sit in
          // the registry with its control thread alive and never be reachable again.
          if (existing == null) {
            transports.remove(key)
            transport.close()
          }
          throw t
        }
      }
    }

    AsyncFunction("trustHostKey") { connectionId: String, fingerprintSha256: String ->
      val transport =
        synchronized(registryLock) {
          val channel = channels[connectionId] ?: throw UnknownConnectionException(connectionId)
          transports[channel.transportKey]
        } ?: throw UnknownConnectionException(connectionId)
      if (!transport.trustHostKey(fingerprintSha256)) {
        throw NoPendingHostKeyException(connectionId)
      }
    }

    AsyncFunction("write") { connectionId: String, base64: String, promise: Promise ->
      val channel = channels[connectionId]
      if (channel == null) {
        promise.reject(UnknownConnectionException(connectionId))
        return@AsyncFunction
      }
      channel.write(
        base64,
        onWritten = { promise.resolve(null) },
        onFailed = { failure -> promise.reject(SshWriteException(failure)) },
      )
    }

    // Closing an id that is already gone is a no-op, so `close` stays idempotent from JavaScript.
    AsyncFunction("close") { connectionId: String ->
      channels[connectionId]?.close()
      Unit
    }

    // Unlike the automatic start in [startServiceIfNeeded], a failure here is reported: JavaScript
    // asked for the service explicitly and needs to know it did not appear.
    AsyncFunction("startForegroundService") { title: String, body: String ->
      serviceTitle = title
      serviceBody = body
      ConnectionService.start(context, title, body)
      serviceRunning = true
    }

    AsyncFunction("stopForegroundService") { stopService() }

    // The text of the ongoing notification (§6.3): kept for the next automatic start and, while
    // the service is up, re-posted in place. No service start happens here — Android 12+ may
    // refuse one from the background, and a connection reaching `connected` is often exactly there.
    AsyncFunction("setServiceNotification") { title: String, body: String ->
      serviceTitle = title
      serviceBody = body
      if (serviceRunning) {
        ConnectionService.update(context, title, body)
      }
    }

    // Scheduling a token that is already pending replaces its deadline.
    AsyncFunction("scheduleWake") { token: String, delayMs: Double ->
      val runnable =
        object : Runnable {
          override fun run() {
            // Only the runnable still registered for the token fires: a reschedule that raced
            // this one on the way out of the queue has replaced it.
            if (synchronized(wakes) { wakes.remove(token, this) }) {
              runCatching { sendEvent(WAKE_EVENT_NAME, mapOf("token" to token)) }
            }
          }
        }
      synchronized(wakes) { wakes.put(token, runnable)?.let(wakeHandler::removeCallbacks) }
      wakeHandler.postDelayed(runnable, delayMs.toLong().coerceAtLeast(0))
      Unit
    }

    // A token that is not pending — already fired, or never scheduled — is a no-op.
    AsyncFunction("cancelWake") { token: String ->
      synchronized(wakes) { wakes.remove(token) }?.let(wakeHandler::removeCallbacks)
      Unit
    }

    OnDestroy {
      ConnectionService.onDisconnectRequested = null
      ConnectionService.onStopped = null
      closeAllChannels()
      stopService()
      synchronized(wakes) { wakes.clear() }
      wakeHandler.removeCallbacksAndMessages(null)
    }
  }

  // -----------------------------------------------------------------------------------------

  /**
   * Never throws: this runs on an IO thread that is in the middle of a connection, and letting an
   * exception out — a lost react context, say — would be misread as the connection failing.
   */
  private fun dispatch(payload: Map<String, Any?>) {
    if (payload["type"] == "connected") {
      runCatching { startServiceIfNeeded() }
    }
    runCatching { sendEvent(EVENT_NAME, payload) }
  }

  private fun onChannelTerminated(channel: SshChannel, reason: String, exitCode: Int?) {
    var doomed: SshTransport? = null
    var idle = false
    synchronized(registryLock) {
      channels.remove(channel.connectionId, channel)
      val transport = transports[channel.transportKey]
      if (transport != null && transport.release(channel)) {
        transports.remove(channel.transportKey)
        doomed = transport
      }
      idle = channels.isEmpty()
    }
    // Outside the lock: closing an SSH client joins sshj's own threads, and a sibling channel may
    // be inside onChannelTerminated at the same moment.
    doomed?.close()
    if (idle && (reason in NON_RETRYABLE_REASONS || exitCode == EXIT_COMMAND_NOT_FOUND)) {
      stopService()
    }
  }

  private fun disconnectFromNotification() {
    runCatching { sendEvent(DISCONNECT_EVENT_NAME, mapOf<String, Any?>()) }
    closeAllChannels()
    stopService()
  }

  private fun closeAllChannels() {
    channels.values.toList().forEach { it.close() }
  }

  private fun startServiceIfNeeded() {
    if (serviceRunning) {
      return
    }
    val title =
      serviceTitle ?: context.applicationInfo.loadLabel(context.packageManager).toString()
    // Android 12+ refuses some background foreground-service starts. The process is already alive
    // in that case, so failing here costs nothing but the notification, and the flag stays false
    // so the next connection tries again.
    serviceRunning = runCatching { ConnectionService.start(context, title, serviceBody) }.isSuccess
  }

  private fun stopService() {
    if (!serviceRunning) {
      return
    }
    serviceRunning = false
    runCatching { ConnectionService.stop(context) }
  }

  private fun <T> translatingKeyStoreErrors(block: () -> T): T =
    try {
      block()
    } catch (e: SshKeyStoreException) {
      throw SshKeyException(e)
    }
}

/** The `SshTarget` object from design doc §6.1, as it arrives from JavaScript. */
class SshTarget : Record {
  @Field val host: String = ""

  @Field val port: Int = 22

  @Field val user: String = ""
}

internal class SshKeyException(cause: SshKeyStoreException) :
  CodedException(cause.message ?: "The SSH key on this device could not be used.", cause)

internal class ConnectionAlreadyOpenException(connectionId: String) :
  CodedException("Connection $connectionId is already open.")

internal class UnknownConnectionException(connectionId: String) :
  CodedException("There is no connection with id $connectionId.")

internal class NoPendingHostKeyException(connectionId: String) :
  CodedException("Connection $connectionId is not waiting for a host key decision.")

internal class SshWriteException(cause: Throwable) :
  CodedException(cause.message ?: "Writing to the connection failed.", cause)
