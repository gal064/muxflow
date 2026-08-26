package dev.muxflow.ssh

import java.io.IOException
import java.io.InputStreamReader
import android.util.Base64
import java.io.OutputStream
import java.security.KeyPair
import java.security.PublicKey
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import net.schmizz.keepalive.KeepAliveProvider
import net.schmizz.sshj.DefaultConfig
import net.schmizz.sshj.SSHClient
import net.schmizz.sshj.common.KeyType
import net.schmizz.sshj.connection.channel.direct.Session
import net.schmizz.sshj.transport.verification.HostKeyVerifier
import net.schmizz.sshj.userauth.UserAuthException

/**
 * One SSH connection running one remote command, with its own IO thread.
 *
 * The session knows nothing about the Muxflow protocol: it authenticates with the phone's key,
 * runs [command] on a channel with no PTY, and moves bytes (design doc §6.2). Everything that can
 * block — the TCP connect, the host key decision, reads, writes — happens off the JavaScript
 * thread: [start] owns a dedicated IO thread, stderr gets its own reader thread, and [write] hands
 * work to a single-threaded executor so writes stay ordered without blocking the caller.
 */
internal class SshSession(
  val connectionId: String,
  private val host: String,
  private val port: Int,
  private val user: String,
  private val command: String,
  private val trustedFingerprint: String?,
  private val keyPair: KeyPair,
  private val emitEvent: (Map<String, Any?>) -> Unit,
  private val onTerminated: (SshSession, String, Int?) -> Unit,
) {
  private companion object {
    const val CONNECT_TIMEOUT_MS = 10_000
    const val KEEP_ALIVE_INTERVAL_SECONDS = 15
    const val STDOUT_CHUNK_BYTES = 64 * 1024
    const val HOST_KEY_DECISION_TIMEOUT_SECONDS = 60L
    const val EXIT_STATUS_TIMEOUT_SECONDS = 5L

    /** Reasons a close may still be relabelled as a deliberate client-side disconnect. */
    val OVERRIDABLE_REASONS = setOf(CloseReason.EXITED, CloseReason.NETWORK_LOST, CloseReason.CONNECT_FAILED)
  }

  /** The exact reason strings the TypeScript facade accepts (design doc §6.1). */
  object CloseReason {
    const val HOST_KEY_NOT_TRUSTED = "hostKeyNotTrusted"
    const val HOST_KEY_MISMATCH = "hostKeyMismatch"
    const val AUTH_FAILED = "authFailed"
    const val CONNECT_FAILED = "connectFailed"
    const val EXITED = "exited"
    const val CLOSED_BY_CLIENT = "closedByClient"
    const val NETWORK_LOST = "networkLost"
  }

  private val hostKeyDecision = ArrayBlockingQueue<Boolean>(1)
  private val terminated = AtomicBoolean(false)
  private val writer: ExecutorService =
    Executors.newSingleThreadExecutor { runnable ->
      Thread(runnable, "muxflow-ssh-write-$connectionId").apply { isDaemon = true }
    }

  @Volatile private var closeRequested = false
  @Volatile private var verifierReason: String? = null
  @Volatile private var pendingFingerprint: String? = null
  @Volatile private var client: SSHClient? = null
  @Volatile private var stdin: OutputStream? = null
  @Volatile private var commandChannel: Session.Command? = null

  fun start() {
    Thread({ run() }, "muxflow-ssh-$connectionId").apply { isDaemon = true }.start()
  }

  /**
   * Answers a pending `hostKey` event. Returns false when no decision is outstanding for this
   * fingerprint, which means the caller answered a stale or unknown prompt.
   */
  fun trustHostKey(fingerprintSha256: String): Boolean {
    if (pendingFingerprint != fingerprintSha256) {
      return false
    }
    return hostKeyDecision.offer(true)
  }

  /**
   * Queues bytes for the remote command's stdin. Returns without waiting for the write; [onWritten]
   * runs on the writer thread once the bytes have been flushed, or [onFailed] if they could not be.
   */
  fun write(base64: String, onWritten: () -> Unit, onFailed: (Throwable) -> Unit) {
    val out = stdin
    if (out == null || closeRequested) {
      onFailed(IOException("Connection $connectionId is not open for writing."))
      return
    }
    try {
      writer.execute {
        try {
          val bytes = Base64.decode(base64, Base64.DEFAULT)
          out.write(bytes)
          out.flush()
          onWritten()
        } catch (t: Throwable) {
          onFailed(t)
        }
      }
    } catch (e: RejectedExecutionException) {
      onFailed(IOException("Connection $connectionId is closing.", e))
    }
  }

  /** Closes stdin, then the channel, then the client. Safe to call more than once. */
  fun close() {
    closeRequested = true
    // Release a verifier that is still waiting for a trust decision.
    hostKeyDecision.offer(false)
    writer.shutdownNow()
    closeQuietly { stdin?.close() }
    closeQuietly { commandChannel?.close() }
    closeQuietly { client?.close() }
  }

  // -----------------------------------------------------------------------------------------
  // IO thread
  // -----------------------------------------------------------------------------------------

  private fun run() {
    var reason: String
    var connected = false
    var channel: Session.Command? = null
    var sshClient: SSHClient? = null
    try {
      SshSecurity.ensureBouncyCastle()
      val config = DefaultConfig().apply { keepAliveProvider = KeepAliveProvider.KEEP_ALIVE }
      sshClient = SSHClient(config)
      client = sshClient
      sshClient.addHostKeyVerifier(hostKeyVerifier)
      sshClient.connectTimeout = CONNECT_TIMEOUT_MS
      // No read timeout: an idle bridge is normal, and the keepalive below detects a dead peer.
      sshClient.timeout = 0
      sshClient.useCompression()
      sshClient.connect(host, port)
      // The desktop uses ServerAliveInterval=15 / ServerAliveCountMax=3 (design doc §3).
      sshClient.connection.keepAlive.keepAliveInterval = KEEP_ALIVE_INTERVAL_SECONDS
      sshClient.authPublickey(user, sshClient.loadKeys(keyPair))

      val session = sshClient.startSession()
      // Deliberately no PTY: the remote command speaks a binary framed protocol on stdin/stdout,
      // and a PTY would rewrite those bytes.
      channel = session.exec(command)
      commandChannel = channel
      stdin = channel.outputStream
      connected = true
      emitEvent(mapOf("type" to "connected", "connectionId" to connectionId))
      startStderrPump(channel)
      pumpStdout(channel)
      reason = CloseReason.EXITED
    } catch (t: Throwable) {
      reason = classify(t, connected)
    }

    if (closeRequested && reason in OVERRIDABLE_REASONS) {
      reason = CloseReason.CLOSED_BY_CLIENT
    }
    finish(reason, shutdown(channel, sshClient))
  }

  private fun pumpStdout(channel: Session.Command) {
    val stdout = channel.inputStream
    val buffer = ByteArray(STDOUT_CHUNK_BYTES)
    while (true) {
      val read = stdout.read(buffer)
      if (read < 0) {
        return
      }
      if (read > 0) {
        val chunk = if (read == buffer.size) buffer else buffer.copyOf(read)
        emitEvent(
          mapOf(
            "type" to "data",
            "connectionId" to connectionId,
            "base64" to Base64.encodeToString(chunk, Base64.NO_WRAP),
          )
        )
      }
    }
  }

  private fun startStderrPump(channel: Session.Command) {
    Thread(
        {
          try {
            InputStreamReader(channel.errorStream, Charsets.UTF_8).buffered().use { reader ->
              while (true) {
                val line = reader.readLine() ?: return@Thread
                emitEvent(
                  mapOf("type" to "stderr", "connectionId" to connectionId, "text" to line)
                )
              }
            }
          } catch (ignored: Throwable) {
            // stderr is diagnostics only; the stdout pump owns the connection's fate.
          }
        },
        "muxflow-ssh-stderr-$connectionId",
      )
      .apply { isDaemon = true }
      .start()
  }

  private fun classify(failure: Throwable, connected: Boolean): String {
    verifierReason?.let {
      return it
    }
    if (generateSequence(failure) { it.cause }.any { it is UserAuthException }) {
      return CloseReason.AUTH_FAILED
    }
    return if (connected) CloseReason.NETWORK_LOST else CloseReason.CONNECT_FAILED
  }

  /** Tears the connection down and returns the remote command's exit status when it is known. */
  private fun shutdown(channel: Session.Command?, sshClient: SSHClient?): Int? {
    if (channel != null) {
      try {
        channel.join(EXIT_STATUS_TIMEOUT_SECONDS, TimeUnit.SECONDS)
      } catch (ignored: Throwable) {
        // The channel may already be gone; the exit status is simply unknown then.
      }
      closeQuietly { channel.close() }
    }
    val exitCode =
      try {
        channel?.exitStatus
      } catch (ignored: Throwable) {
        null
      }
    writer.shutdownNow()
    closeQuietly { sshClient?.close() }
    return exitCode
  }

  private fun finish(reason: String, exitCode: Int?) {
    if (!terminated.compareAndSet(false, true)) {
      return
    }
    emitEvent(
      mapOf(
        "type" to "closed",
        "connectionId" to connectionId,
        "exitCode" to exitCode,
        "reason" to reason,
      )
    )
    onTerminated(this, reason, exitCode)
  }

  private inline fun closeQuietly(block: () -> Unit) {
    try {
      block()
    } catch (ignored: Throwable) {
      // Closing is best effort: every path here is already on its way down.
    }
  }

  private val hostKeyVerifier =
    object : HostKeyVerifier {
      override fun verify(hostname: String, port: Int, key: PublicKey): Boolean {
        val fingerprint = SshKeyStore.sha256Fingerprint(key)
        if (trustedFingerprint != null) {
          if (trustedFingerprint == fingerprint) {
            return true
          }
          verifierReason = CloseReason.HOST_KEY_MISMATCH
          return false
        }
        pendingFingerprint = fingerprint
        emitEvent(
          mapOf(
            "type" to "hostKey",
            "connectionId" to connectionId,
            "algorithm" to KeyType.fromKey(key).toString(),
            "fingerprintSha256" to fingerprint,
          )
        )
        val trusted =
          try {
            hostKeyDecision.poll(HOST_KEY_DECISION_TIMEOUT_SECONDS, TimeUnit.SECONDS)
          } catch (e: InterruptedException) {
            Thread.currentThread().interrupt()
            null
          }
        if (trusted != true) {
          verifierReason = CloseReason.HOST_KEY_NOT_TRUSTED
          return false
        }
        return true
      }

      // The phone keeps its own trust store in JavaScript, so there is no known_hosts file to
      // narrow the offered host key algorithms with.
      override fun findExistingAlgorithms(hostname: String, port: Int): List<String> = emptyList()
    }
}
