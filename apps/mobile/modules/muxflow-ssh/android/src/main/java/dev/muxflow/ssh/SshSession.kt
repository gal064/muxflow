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
import net.schmizz.keepalive.KeepAliveRunner
import net.schmizz.sshj.DefaultConfig
import net.schmizz.sshj.SSHClient
import net.schmizz.sshj.common.KeyType
import net.schmizz.sshj.connection.channel.direct.Session
import net.schmizz.sshj.transport.verification.HostKeyVerifier
import net.schmizz.sshj.userauth.UserAuthException
import net.schmizz.sshj.userauth.keyprovider.KeyPairWrapper
import net.schmizz.sshj.userauth.method.AuthMethod
import net.schmizz.sshj.userauth.method.AuthNone
import net.schmizz.sshj.userauth.method.AuthPublickey

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
private const val KEEP_ALIVE_MAX_COUNT = 3
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
  keyPair: KeyPair?,
  private val emitEvent: (Map<String, Any?>) -> Unit,
) {
  private val control =
    Executors.newSingleThreadExecutor { runnable ->
      Thread(runnable, "muxflow-ssh-control-$key").apply { isDaemon = true }
    }
  private val channels = ConcurrentHashMap<String, SshChannel>()
  private val hostKeyDecision = ArrayBlockingQueue<Boolean>(1)

  @Volatile private var client: SSHClient? = null

  /**
   * The key the next authentication offers. Refreshed on every [open], so a key generated after a
   * refused login (or regenerated while connected) is what the next attempt sends.
   */
  @Volatile private var keyPair: KeyPair? = keyPair

  /**
   * The client [ensureConnected] is currently connecting or authenticating. Closing it from another
   * thread is the only way to end that wait early: Tailscale SSH in check mode holds the login open
   * until the user signs in on another device, and the transport has no read timeout.
   */
  @Volatile private var connecting: SSHClient? = null
  @Volatile private var pendingFingerprint: String? = null

  /** The fingerprint this transport actually authenticated against, once it has one. */
  @Volatile private var verifiedFingerprint: String? = null
  @Volatile private var verifierReason: String? = null

  /**
   * Registers a channel and schedules it on the shared transport. The caller must serialise [open]
   * against [release] so that "no channels left" cannot race a new one.
   */
  fun open(
    connectionId: String,
    command: String,
    trustedFingerprint: String?,
    keyPair: KeyPair?,
    onTerminated: (SshChannel, String, Int?) -> Unit,
  ): SshChannel {
    this.keyPair = keyPair
    val channel =
      SshChannel(connectionId, key, command, emitEvent, onTerminated, ::onChannelCloseRequested)
    channels[connectionId] = channel
    try {
      control.execute {
        try {
          channel.startOn(ensureConnected(connectionId, trustedFingerprint))
        } catch (t: Throwable) {
          channel.failBeforeStart((t as? TransportSetupException)?.reason ?: CloseReason.CONNECT_FAILED)
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
    if (channels.values.any { !it.isCloseRequested }) {
      return
    }
    if (pendingFingerprint != null) {
      hostKeyDecision.offer(false)
    }
    // Ends a connect or authentication still in flight; `ensureConnected` then fails and the
    // closing channel finishes as `closedByClient` instead of parking the control thread forever.
    closeQuietly { connecting?.close() }
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
    client?.let { existing ->
      if (existing.isConnected && existing.isAuthenticated) {
        // The verifier does not run again for a channel that joins a live transport, so the pin
        // this caller brought is checked here instead. §14 makes a mismatch a hard failure, and
        // silently inheriting someone else's trust decision would be exactly that mismatch.
        val verified = verifiedFingerprint
        if (trustedFingerprint != null && verified != null && trustedFingerprint != verified) {
          throw TransportSetupException(CloseReason.HOST_KEY_MISMATCH, null)
        }
        return existing
      }
      // A dead client still owns a socket, a reader thread and a keepalive thread.
      closeQuietly { existing.close() }
      client = null
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
      val keepAlive = fresh.connection.keepAlive
      keepAlive.keepAliveInterval = KEEP_ALIVE_INTERVAL_SECONDS
      (keepAlive as? KeepAliveRunner)?.maxAliveCount = KEEP_ALIVE_MAX_COUNT
      connecting = fresh
      fresh.connect(host, port)
      fresh.auth(user, authMethods())
    } catch (t: Throwable) {
      connecting = null
      val reason =
        verifierReason
          ?: if (generateSequence(t) { it.cause }.any { it is UserAuthException }) {
            CloseReason.AUTH_FAILED
          } else {
            CloseReason.CONNECT_FAILED
          }
      closeQuietly { fresh.close() }
      // Deliberately not remembered on the transport: a later channel gets its own attempt rather
      // than inheriting the reason an earlier, unrelated attempt failed with.
      throw TransportSetupException(reason, t)
    }
    connecting = null
    client = fresh
    return fresh
  }

  /**
   * The same order the OpenSSH client uses. `none` goes first: Tailscale SSH has already
   * authenticated the phone by its tailnet identity and accepts it outright, so a tailnet host
   * needs no key on the phone at all. A regular sshd answers `none` with the methods it does
   * accept, and sshj only moves on to `publickey` when the server listed it.
   */
  private fun authMethods(): List<AuthMethod> {
    val methods = mutableListOf<AuthMethod>(AuthNone())
    keyPair?.let { methods.add(AuthPublickey(KeyPairWrapper(it))) }
    return methods
  }

  private fun hostKeyVerifier(connectionId: String, trustedFingerprint: String?) =
    object : HostKeyVerifier {
      override fun verify(hostname: String, port: Int, key: PublicKey): Boolean {
        val fingerprint = SshKeyStore.sha256Fingerprint(key)
        val trusted = trustedFingerprint ?: verifiedFingerprint
        if (trusted != null) {
          if (trusted == fingerprint) {
            verifiedFingerprint = fingerprint
            return true
          }
          verifierReason = CloseReason.HOST_KEY_MISMATCH
          return false
        }
        // Only the very first key exchange can reach here. sshj calls the verifier again on every
        // rekey, and the branch above answers those from `verifiedFingerprint` rather than
        // prompting a second time and stalling the reader thread for 60 s.
        // Nothing may be left over from an earlier decision: a stale answer here would approve a
        // key the user never saw.
        hostKeyDecision.clear()
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
            // Stop accepting answers for this prompt before draining, so a duplicate `trustHostKey`
            // cannot slip a stale approval into the queue behind us.
            pendingFingerprint = null
            hostKeyDecision.clear()
          }
        if (decision != true) {
          verifierReason = CloseReason.HOST_KEY_NOT_TRUSTED
          return false
        }
        verifiedFingerprint = fingerprint
        return true
      }

      // The phone keeps its own trust store in JavaScript, so there is no known_hosts file to
      // narrow the offered host key algorithms with.
      override fun findExistingAlgorithms(hostname: String, port: Int): List<String> = emptyList()
    }
}

/** Carries the close reason a failed transport setup should report to its waiting channel. */
private class TransportSetupException(val reason: String, cause: Throwable?) :
  IOException("SSH transport setup failed: $reason", cause)

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
  private val closing = AtomicBoolean(false)
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
    if (!closing.compareAndSet(false, true)) {
      return
    }
    // Both of these block on the network: sshj flushes stdin (which waits for remote window space)
    // and then waits up to the connection's 30 s timeout for the channel close to be confirmed.
    // The callers are the JavaScript async queue and the notification's Disconnect action on the
    // main thread, so neither may wait for them.
    Thread(
        {
          closeQuietly { stdin?.close() }
          closeQuietly { channel?.close() }
        },
        "muxflow-ssh-close-$connectionId",
      )
      .apply { isDaemon = true }
      .start()
  }

  private fun run(client: SSHClient) {
    var reason = CloseReason.CONNECT_FAILED
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
    } finally {
      // In a finally so that no failure on the way down can leave JavaScript without a `closed`
      // event, holding the connectionId — and its transport — open forever.
      if (closeRequested) {
        reason = CloseReason.CLOSED_BY_CLIENT
      }
      val exitCode =
        try {
          shutdown(opened)
        } catch (t: Throwable) {
          null
        }
      finish(reason, exitCode)
    }
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
    // §12 builds the error strip from the first stderr line, so that line has to reach JavaScript
    // before `closed` does. Closing the stream ends the pump's blocking read rather than leaving it
    // free to emit a `stderr` event after the connection is already gone.
    closeQuietly { opened.errorStream.close() }
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
