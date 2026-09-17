package dev.muxflow.ssh

import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/**
 * Keeps the app's process — and with it the JavaScript runtime that owns the protocol client —
 * alive while an SSH connection is up, behind one ongoing "Connected to …" notification
 * (design doc §6.3 / D9).
 *
 * There is deliberately no protocol logic here. The service only holds a wake lock and shows the
 * notification; its "Disconnect" action calls back into [MuxflowSshModule], which tells JavaScript
 * (`onDisconnectRequested`) and then closes the connections so each open one also sees a normal
 * `closed` event. The text is JavaScript's: [MuxflowSshModule] hands it in at start and [update]s
 * it in place as the connection comes and goes.
 */
class ConnectionService : Service() {
  companion object {
    /** Notification channel from design doc §6.3: importance LOW, no sound. */
    private const val CHANNEL_ID = "connection"
    private const val CHANNEL_NAME = "Connection"
    private const val NOTIFICATION_ID = 4711

    private const val ACTION_START = "dev.muxflow.ssh.action.START"
    private const val ACTION_STOP = "dev.muxflow.ssh.action.STOP"
    const val ACTION_DISCONNECT = "dev.muxflow.ssh.action.DISCONNECT"
    private const val WAKE_LOCK_TAG = "muxflow:ssh-connection"

    /** Set by [MuxflowSshModule] while it is alive; invoked by the notification's Disconnect action. */
    @Volatile var onDisconnectRequested: (() -> Unit)? = null

    /** Invoked whenever the service goes away, including when the system stops it on its own. */
    @Volatile var onStopped: (() -> Unit)? = null

    /**
     * True from `startForeground` in a START until `onDestroy`. [MuxflowSshModule]'s own flag flips
     * when it *sends* a start or a stop, which is before either has run; [update] needs the truth,
     * or it would post on a channel that does not exist yet (dropped) or after the notification was
     * removed (an orphan with no service behind it). Read and written on the main thread only.
     */
    private var live = false

    /**
     * The notification's text, shared by [start] and [update] rather than carried in the START
     * intent: an update that lands between the intent being sent and `onStartCommand` running
     * would otherwise be lost, and the start would draw the text it had superseded.
     */
    @Volatile private var title: String? = null
    @Volatile private var body: String = ""

    private val main = Handler(Looper.getMainLooper())

    fun start(context: Context, title: String, body: String) {
      this.title = title
      this.body = body
      val intent = Intent(context, ConnectionService::class.java).setAction(ACTION_START)
      ContextCompat.startForegroundService(context, intent)
    }

    /**
     * Replaces the text of the notification a running service already shows. Posting under the
     * service's own id updates it in place — no service start, so unlike [start] this is always
     * allowed from the background. Runs on the main thread, where `onStartCommand` and `onDestroy`
     * also run, so it cannot slip in between the notification being removed and [live] noticing.
     * When the service is not up, the text is only kept for its next start.
     */
    fun update(context: Context, title: String, body: String) {
      this.title = title
      this.body = body
      main.post { if (live) post(context.applicationContext) }
    }

    // The permission is declared and requested (§13); without it Android hides the whole
    // foreground-service notification anyway, so the update has nothing to be dropped from.
    @SuppressLint("MissingPermission")
    private fun post(context: Context) {
      createChannel(context)
      val notification = buildNotification(context, title ?: serviceLabel(context), body)
      NotificationManagerCompat.from(context).notify(NOTIFICATION_ID, notification)
    }

    private fun createChannel(context: Context) {
      val manager = context.getSystemService(NotificationManager::class.java) ?: return
      if (manager.getNotificationChannel(CHANNEL_ID) != null) {
        return
      }
      val channel =
        NotificationChannel(CHANNEL_ID, CHANNEL_NAME, NotificationManager.IMPORTANCE_LOW).apply {
          setShowBadge(false)
          setSound(null, null)
          enableVibration(false)
        }
      manager.createNotificationChannel(channel)
    }

    private fun serviceLabel(context: Context): String =
      context.applicationInfo.loadLabel(context.packageManager).toString()

    /** The one place the notification is built, so a start and an [update] cannot drift apart. */
    private fun buildNotification(context: Context, title: String, body: String): Notification {
      val flags = PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
      val disconnectIntent =
        PendingIntent.getService(
          context,
          1,
          Intent(context, ConnectionService::class.java).setAction(ACTION_DISCONNECT),
          flags,
        )
      val builder =
        NotificationCompat.Builder(context, CHANNEL_ID)
          .setSmallIcon(context.applicationInfo.icon)
          .setContentTitle(title)
          .setContentText(body)
          .setOngoing(true)
          .setSilent(true)
          .setShowWhen(false)
          .setCategory(NotificationCompat.CATEGORY_SERVICE)
          .setPriority(NotificationCompat.PRIORITY_LOW)
          .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
          .addAction(0, "Disconnect", disconnectIntent)
      context.packageManager.getLaunchIntentForPackage(context.packageName)?.let { launch ->
        builder.setContentIntent(PendingIntent.getActivity(context, 0, launch, flags))
      }
      return builder.build()
    }

    /**
     * Stops the service through itself rather than with `stopService`.
     *
     * Android kills the whole process with `ForegroundServiceDidNotStartInTimeException` when a
     * service that was started with `startForegroundService()` is stopped from outside before it
     * has called `startForeground()`. That window is not theoretical here: a helper that is not
     * installed exits 127 within milliseconds of the channel reporting `connected`, so the start
     * and the stop land in the same tick (design doc §12, "Helper missing"). Delivering the stop as
     * an intent guarantees `onStartCommand` runs, and the service then enters the foreground and
     * leaves again in one turn — which honours the contract in every ordering.
     */
    fun stop(context: Context) {
      val intent = Intent(context, ConnectionService::class.java).setAction(ACTION_STOP)
      try {
        ContextCompat.startForegroundService(context, intent)
      } catch (t: Throwable) {
        // Android 12+ can refuse a foreground start from the background. Nothing was started in
        // that case either, so the plain stop is both safe and the only way left to be sure.
        context.stopService(Intent(context, ConnectionService::class.java))
      }
    }
  }

  private var wakeLock: PowerManager.WakeLock? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_DISCONNECT) {
      onDisconnectRequested?.invoke()
      stopSelf()
      return START_NOT_STICKY
    }
    if (intent?.action == ACTION_STOP) {
      // Enter the foreground before leaving it: see [stop]. `onDestroy` takes the notification
      // down again in the same main-thread turn, so nothing is drawn.
      createChannel(this)
      startInForeground(buildNotification(this, serviceLabel(this), ""))
      stopSelf()
      return START_NOT_STICKY
    }
    createChannel(this)
    startInForeground(buildNotification(this, title ?: serviceLabel(this), body))
    live = true
    acquireWakeLock()
    return START_NOT_STICKY
  }

  override fun onDestroy() {
    live = false
    onStopped?.invoke()
    releaseWakeLock()
    ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
    super.onDestroy()
  }

  private fun startInForeground(notification: Notification) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
      startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
    } else {
      startForeground(NOTIFICATION_ID, notification)
    }
  }

  private fun acquireWakeLock() {
    if (wakeLock?.isHeld == true) {
      return
    }
    val power = getSystemService(PowerManager::class.java) ?: return
    wakeLock =
      power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, WAKE_LOCK_TAG).apply {
        setReferenceCounted(false)
        // Held with no timeout on purpose: the connection, not a clock, decides when it ends.
        @Suppress("WakelockTimeout") acquire()
      }
  }

  private fun releaseWakeLock() {
    wakeLock?.let { held ->
      if (held.isHeld) {
        held.release()
      }
    }
    wakeLock = null
  }
}
