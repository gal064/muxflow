package dev.muxflow.ssh

import android.content.Context
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
 * It opens an exec channel to `muxflow-host bridge --stdio` and moves bytes; it knows nothing about
 * the protocol those bytes carry. Every event reaches JavaScript through the single `onSshEvent`
 * event, discriminated by `type`, which is what `src/ssh/MuxflowSsh.ts` fans out into the typed
 * union in §6.1.
 */
class MuxflowSshModule : Module() {
  private companion object {
    const val EVENT_NAME = "onSshEvent"

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
        SshSession.CloseReason.CLOSED_BY_CLIENT,
        SshSession.CloseReason.AUTH_FAILED,
        SshSession.CloseReason.HOST_KEY_MISMATCH,
        SshSession.CloseReason.HOST_KEY_NOT_TRUSTED,
      )
  }

  private val sessions = ConcurrentHashMap<String, SshSession>()

  @Volatile private var serviceTitle: String? = null
  @Volatile private var serviceBody: String = ""
  @Volatile private var serviceRunning = false

  private val context: Context
    get() = appContext.reactContext ?: throw Exceptions.ReactContextLost()

  override fun definition() = ModuleDefinition {
    Name("MuxflowSsh")

    Events(EVENT_NAME)

    OnCreate {
      SshSecurity.ensureBouncyCastle()
      ConnectionService.onDisconnectRequested = { disconnectFromNotification() }
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
      if (sessions.containsKey(connectionId)) {
        throw ConnectionAlreadyOpenException(connectionId)
      }
      val keyPair = translatingKeyStoreErrors { SshKeyStore.keyPair(context) }
      val session =
        SshSession(
          connectionId = connectionId,
          host = target.host,
          port = target.port,
          user = target.user,
          command = command,
          trustedFingerprint = trustedHostKeyFingerprint,
          keyPair = keyPair,
          emitEvent = ::dispatch,
          onTerminated = ::onSessionTerminated,
        )
      // A racing `connect` for the same id must not orphan a session on its own IO thread.
      if (sessions.putIfAbsent(connectionId, session) != null) {
        throw ConnectionAlreadyOpenException(connectionId)
      }
      session.start()
    }

    AsyncFunction("trustHostKey") { connectionId: String, fingerprintSha256: String ->
      val session = sessions[connectionId] ?: throw UnknownConnectionException(connectionId)
      if (!session.trustHostKey(fingerprintSha256)) {
        throw NoPendingHostKeyException(connectionId)
      }
    }

    AsyncFunction("write") { connectionId: String, base64: String, promise: Promise ->
      val session = sessions[connectionId]
      if (session == null) {
        promise.reject(UnknownConnectionException(connectionId))
        return@AsyncFunction
      }
      session.write(
        base64,
        onWritten = { promise.resolve(null) },
        onFailed = { failure -> promise.reject(SshWriteException(failure)) },
      )
    }

    AsyncFunction("close") { connectionId: String -> sessions[connectionId]?.close() }

    AsyncFunction("startForegroundService") { title: String, body: String ->
      serviceTitle = title
      serviceBody = body
      serviceRunning = true
      ConnectionService.start(context, title, body)
    }

    AsyncFunction("stopForegroundService") { stopService() }

    OnDestroy {
      ConnectionService.onDisconnectRequested = null
      closeAllSessions()
      stopService()
    }
  }

  // -----------------------------------------------------------------------------------------

  private fun dispatch(payload: Map<String, Any?>) {
    if (payload["type"] == "connected") {
      startServiceIfNeeded()
    }
    // The module can be torn down while an IO thread is still draining; a dropped event then is
    // preferable to crashing that thread.
    runCatching { sendEvent(EVENT_NAME, payload) }
  }

  private fun onSessionTerminated(session: SshSession, reason: String, exitCode: Int?) {
    sessions.remove(session.connectionId, session)
    if (sessions.isEmpty() &&
      (reason in NON_RETRYABLE_REASONS || exitCode == EXIT_COMMAND_NOT_FOUND)
    ) {
      stopService()
    }
  }

  private fun disconnectFromNotification() {
    closeAllSessions()
    stopService()
  }

  private fun closeAllSessions() {
    sessions.values.toList().forEach { it.close() }
  }

  private fun startServiceIfNeeded() {
    if (serviceRunning) {
      return
    }
    val title =
      serviceTitle ?: context.applicationInfo.loadLabel(context.packageManager).toString()
    serviceRunning = true
    // Android 12+ refuses some background foreground-service starts. The process is already alive
    // in that case, so failing here costs nothing but the notification.
    runCatching { ConnectionService.start(context, title, serviceBody) }
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
