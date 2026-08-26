package dev.muxflow.ssh

import android.util.Base64
import java.io.IOException
import java.io.InputStreamReader
import java.io.OutputStream
import java.security.KeyPair
import java.security.PublicKey
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ConcurrentHashMap
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

/** The exact close reason strings the TypeScript facade accepts (design doc §6.1). */
internal object CloseReason {
  const val HOST_KEY_NOT_TRUSTED = "hostKeyNotTrusted"
  const val HOST_KEY_MISMATCH = "hostKeyMismatch"
  const val AUTH_FAILED = "authFailed"
  const val CONNECT_FAILED = "connectFailed"
  const val EXITED = "exited"
  const val CLOSED_BY_CLIENT = "closedByClient"
  const val NETWORK_LOST = "networkLost"
}

private const val CONNECT_TIMEOUT_MS = 10_000
private const val KEEP_ALIVE_INTERVAL_SECONDS = 15
private const val STDOUT_CHUNK_BYTES = 64 * 1024
private const val HOST_KEY_DECISION_TIMEOUT_SECONDS = 60L
private const val EXIT_STATUS_TIMEOUT_SECONDS = 5L
private const val STDERR_DRAIN_TIMEOUT_MS = 500L

/**
 * One authenticated SSH connection to one `user@host:port`, shared by every channel opened against
 * that target.
 *
 * The protocol client needs more than one `muxflow-host bridge --stdio` at a time — file bodies are
 * served over a separate "bulk" bridge — and those are extra exec channels on the same transport,
 * not extra logins. A single-threaded control executor serialises connect / authenticate / open, so
 * a burst of `connect` calls produces exactly one TCP connection, one host key prompt and one
 * authentication; each channel then runs on its own IO thread and closes independently.
 */
internal class SshTransport(
  val key: String,
  private val host: String,
  private val port: Int,
  private val user: String,
  private val keyPair: KeyPair,
  private val emitEvent: (Map<String, Any?>) -> Unit,
) {
  private val control =
    Executors.newSingleThreadExecutor { runnable ->
      Thread(runnable, "muxflow-ssh-control-$key").apply { isDaemon = true }
    }
  private val channels = ConcurrentHashMap<String, SshChannel>()
  private val hostKeyDecision = ArrayBlockingQueue<Boolean>(1)

  @Volatile private var client: SSHClient? = null
  @Volatile private var setupFailure: String? = null
  @Volatile private var pendingFingerprint: String? = null
  @Volatile private var acceptedFingerprint: String? = null
  @Volatile private var verifierReason: String? = null

  /**
   * Registers a channel and schedules it on the shared transport. The caller must serialise [open]
   * against [release] so that "no channels left" cannot race a new one.
   */
  fun open(
    connectionId: String,
    command: String,
    trustedFingerprint: String?,
    onTerminated: (SshChannel, String, Int?) -> Unit,
  ): SshChannel {
    val channel =
      SshChannel(connectionId, key, command, emitEvent, onTerminated, ::onChannelCloseRequested)
    channels[connectionId] = channel
    try {
      control.execute {
        try {
          channel.startOn(ensureConnected(connectionId, trustedFingerprint))
        } catch (t: Throwable) {
          channel.failBeforeStart(setupFailure ?: CloseReason.CONNECT_FAILED)
        }
      }
    } catch (e: RejectedExecutionException) {
      channels.remove(connectionId, channel)
      throw e
    }
    return channel
  }

  /** Drops a finished channel and reports whether the transport now has none left. */
  fun release(channel: SshChannel): Boolean {
    channels.remove(channel.connectionId, channel)
    return channels.isEmpty()
  }

  /**
   * Stops waiting for a host key decision once nothing is left that would use the answer, so a
   * disconnect during the trust dialog ends as `closedByClient` instead of sitting out the 60 s.
   */
  private fun onChannelCloseRequested() {
    if (pendingFingerprint != null && channels.values.none { !it.isCloseRequested }) {
      hostKeyDecision.offer(false)
    }
  }

  /** Answers a pending `hostKey` event; false when no decision is outstanding for that fingerprint. */
  fun trustHostKey(fingerprintSha256: String): Boolean {
    if (pendingFingerprint != fingerprintSha256) {
      return false
    }
    return hostKeyDecision.offer(true)
  }

  /** Tears the transport down. Only call this once every channel on it has terminated. */
  fun close() {
    control.shutdownNow()
    // Release a verifier that is still waiting for a trust decision.
    hostKeyDecision.offer(false)
    closeQuietly { client?.close() }
    client = null
  }

  // Runs on the control thread, so only one connection attempt is ever in flight.
  private fun ensureConnected(connectionId: String, trustedFingerprint: String?): SSHClient {
    setupFailure?.let { throw IOException("SSH transport $key already failed: $it") }
    client?.let { existing ->
      if (existing.isConnected && existing.isAuthenticated) {
        // Later channels inherit the host key decision the first one made: their
        // trustedFingerprint argument describes the same, already-verified key.
        return existing
      }
    }
    SshSecurity.ensureBouncyCastle()
    verifierReason = null
    val fresh = SSHClient(DefaultConfig().apply { keepAliveProvider = KeepAliveProvider.KEEP_ALIVE })
    try {
      fresh.addHostKeyVerifier(hostKeyVerifier(connectionId, trustedFingerprint))
      fresh.connectTimeout = CONNECT_TIMEOUT_MS
      // No read timeout: an idle bridge is normal, and the keepalive below detects a dead peer.
      fresh.timeout = 0
      fresh.useCompression()
      // Must be set before connect(): sshj only starts the keepalive thread during connect, and
      // only when the interval is already non-zero. The desktop uses ServerAliveInterval=15 with
      // ServerAliveCountMax=3 (design doc §3), which is what makes a dropped link surface as
      // `networkLost` instead of a read that blocks forever.
      fresh.connection.keepAlive.keepAliveInterval = KEEP_ALIVE_INTERVAL_SECONDS
      fresh.connect(host, port)
      fresh.authPublickey(user, fresh.loadKeys(keyPair))
    } catch (t: Throwable) {
      val reason =
        verifierReason
          ?: if (generateSequence(t) { it.cause }.any { it is UserAuthException }) {
            CloseReason.AUTH_FAILED
          } else {
            CloseReason.CONNECT_FAILED
          }
      setupFailure = reason
      closeQuietly { fresh.close() }
      throw IOException("SSH transport $key failed: $reason", t)
    }
    client = fresh
    return fresh
  }

  private fun hostKeyVerifier(connectionId: String, trustedFingerprint: String?) =
    object : HostKeyVerifier {
      override fun verify(hostname: String, port: Int, key: PublicKey): Boolean {
        val fingerprint = SshKeyStore.sha256Fingerprint(key)
        val trusted = trustedFingerprint ?: acceptedFingerprint
        if (trusted != null) {
          if (trusted == fingerprint) {
            return true
          }
          verifierReason = CloseReason.HOST_KEY_MISMATCH
          return false
        }
        // Only the very first key exchange can reach here. sshj calls the verifier again on every
        // rekey, and the branch above answers those from `acceptedFingerprint` rather than
        // prompting a second time and stalling the reader thread for 60 s.
        pendingFingerprint = fingerprint
        emitEvent(
          mapOf(
            "type" to "hostKey",
            "connectionId" to connectionId,
            "algorithm" to KeyType.fromKey(key).toString(),
            "fingerprintSha256" to fingerprint,
          )
        )
        val decision =
          try {
            hostKeyDecision.poll(HOST_KEY_DECISION_TIMEOUT_SECONDS, TimeUnit.SECONDS)
          } catch (e: InterruptedException) {
            Thread.currentThread().interrupt()
            null
          } finally {
            // A duplicate answer must not sit in the queue and silently approve a later key.
            pendingFingerprint = null
            hostKeyDecision.clear()
          }
        if (decision != true) {
          verifierReason = CloseReason.HOST_KEY_NOT_TRUSTED
          return false
        }
        acceptedFingerprint = fingerprint
        return true
      }

      // The phone keeps its own trust store in JavaScript, so there is no known_hosts file to
      // narrow the offered host key algorithms with.
      override fun findExistingAlgorithms(hostname: String, port: Int): List<String> = emptyList()
    }
}

/**
 * One remote command on one exec channel of an [SshTransport], with its own IO thread.
 *
 * The channel knows nothing about the Muxflow protocol: it runs [command] on a channel with no PTY
 * and moves bytes. Nothing here blocks the JavaScript thread — stdout is read on the channel's own
 * thread, stderr on a second one, and [write] hands work to a single-threaded executor so writes
 * stay ordered without the caller waiting on the socket.
 */
internal class SshChannel(
  val connectionId: String,
  val transportKey: String,
  private val command: String,
  private val emitEvent: (Map<String, Any?>) -> Unit,
  private val onTerminated: (SshChannel, String, Int?) -> Unit,
  private val onCloseRequested: () -> Unit,
) {
  private val terminated = AtomicBoolean(false)
  private val writer: ExecutorService =
    Executors.newSingleThreadExecutor { runnable ->
      Thread(runnable, "muxflow-ssh-write-$connectionId").apply { isDaemon = true }
    }

  @Volatile private var closeRequested = false
  @Volatile private var stdin: OutputStream? = null
  @Volatile private var channel: Session.Command? = null
  @Volatile private var stderrPump: Thread? = null

  val isCloseRequested: Boolean
    get() = closeRequested

  fun startOn(client: SSHClient) {
    Thread({ run(client) }, "muxflow-ssh-$connectionId").apply { isDaemon = true }.start()
  }

  /** Reports a channel that never opened because its transport could not be established. */
  fun failBeforeStart(reason: String) {
    finish(if (closeRequested) CloseReason.CLOSED_BY_CLIENT else reason, null)
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
      writer.execute(WriteTask(base64, out, onWritten, onFailed))
    } catch (e: RejectedExecutionException) {
      onFailed(IOException("Connection $connectionId is closing.", e))
    }
  }

  /**
   * Closes stdin and then this channel. Safe to call more than once, and it deliberately leaves the
   * shared transport alone — other channels on the same host keep running.
   */
  fun close() {
    closeRequested = true
    onCloseRequested()
    cancelQueuedWrites()
    closeQuietly { stdin?.close() }
    closeQuietly { channel?.close() }
  }

  private fun run(client: SSHClient) {
    var reason: String
    var connected = false
    var opened: Session.Command? = null
    try {
      requireStillWanted()
      val session = client.startSession()
      // Deliberately no PTY: the remote command speaks a binary framed protocol on stdin/stdout,
      // and a PTY would rewrite those bytes.
      opened = session.exec(command)
      channel = opened
      stdin = opened.outputStream
      // close() may have run while the channel was being opened, in which case it saw no channel
      // to close and this thread must not go on to pump a connection nobody is listening to.
      requireStillWanted()
      connected = true
      emitEvent(mapOf("type" to "connected", "connectionId" to connectionId))
      startStderrPump(opened)
      pumpStdout(opened)
      reason = CloseReason.EXITED
    } catch (t: Throwable) {
      reason = if (connected) CloseReason.NETWORK_LOST else CloseReason.CONNECT_FAILED
    }
    if (closeRequested) {
      reason = CloseReason.CLOSED_BY_CLIENT
    }
    finish(reason, shutdown(opened))
  }

  private fun requireStillWanted() {
    if (closeRequested) {
      throw IOException("Connection $connectionId was closed before it opened.")
    }
  }

  private fun pumpStdout(opened: Session.Command) {
    val stdout = opened.inputStream
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

  private fun startStderrPump(opened: Session.Command) {
    val pump =
      Thread(
        {
          try {
            InputStreamReader(opened.errorStream, Charsets.UTF_8).buffered().use { reader ->
              while (true) {
                val line = reader.readLine() ?: return@Thread
                emitEvent(
                  mapOf("type" to "stderr", "connectionId" to connectionId, "text" to line)
                )
              }
            }
          } catch (ignored: Throwable) {
            // stderr is diagnostics only; the stdout pump owns the channel's fate.
          }
        },
        "muxflow-ssh-stderr-$connectionId",
      )
    stderrPump = pump
    pump.isDaemon = true
    pump.start()
  }

  /** Closes this channel only and returns the remote command's exit status when it is known. */
  private fun shutdown(opened: Session.Command?): Int? {
    if (opened == null) {
      cancelQueuedWrites()
      return null
    }
    try {
      opened.join(EXIT_STATUS_TIMEOUT_SECONDS, TimeUnit.SECONDS)
    } catch (ignored: Throwable) {
      // The channel may already be gone; the exit status is simply unknown then.
    }
    closeQuietly { opened.close() }
    cancelQueuedWrites()
    // §12 builds the error strip from the first stderr line, so give that line a bounded chance to
    // reach JavaScript before `closed` does.
    try {
      stderrPump?.join(STDERR_DRAIN_TIMEOUT_MS)
    } catch (e: InterruptedException) {
      Thread.currentThread().interrupt()
    }
    return try {
      opened.exitStatus
    } catch (ignored: Throwable) {
      null
    }
  }

  /**
   * Stops the writer and settles every write that never ran. A dropped task would otherwise leave
   * its JavaScript promise pending forever, wedging the facade's per-connection write queue.
   */
  private fun cancelQueuedWrites() {
    for (task in writer.shutdownNow()) {
      (task as? WriteTask)?.cancel(connectionId)
    }
  }

  private fun finish(reason: String, exitCode: Int?) {
    if (!terminated.compareAndSet(false, true)) {
      return
    }
    cancelQueuedWrites()
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

  private class WriteTask(
    private val base64: String,
    private val out: OutputStream,
    private val onWritten: () -> Unit,
    private val onFailed: (Throwable) -> Unit,
  ) : Runnable {
    private val settled = AtomicBoolean(false)

    override fun run() {
      if (!settled.compareAndSet(false, true)) {
        return
      }
      try {
        val bytes = Base64.decode(base64, Base64.DEFAULT)
        out.write(bytes)
        out.flush()
        onWritten()
      } catch (t: Throwable) {
        onFailed(t)
      }
    }

    fun cancel(connectionId: String) {
      if (settled.compareAndSet(false, true)) {
        onFailed(IOException("Connection $connectionId closed before the write was sent."))
      }
    }
  }
}

private inline fun closeQuietly(block: () -> Unit) {
  try {
    block()
  } catch (ignored: Throwable) {
    // Closing is best effort: every path here is already on its way down.
  }
}
