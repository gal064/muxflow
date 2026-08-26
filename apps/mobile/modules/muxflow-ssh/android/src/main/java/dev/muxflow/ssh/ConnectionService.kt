package dev.muxflow.ssh

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/**
 * Keeps the app's process — and with it the JavaScript runtime that owns the protocol client —
 * alive while an SSH connection is up, behind one ongoing "Connected to …" notification
 * (design doc §6.3 / D9).
 *
 * There is deliberately no protocol logic here. The service only holds a wake lock and shows the
 * notification; its "Disconnect" action calls back into [MuxflowSshModule], which closes the
 * connections so JavaScript sees a normal `closed` event.
 */
class ConnectionService : Service() {
  companion object {
    /** Notification channel from design doc §6.3: importance LOW, no sound. */
    private const val CHANNEL_ID = "connection"
    private const val CHANNEL_NAME = "Connection"
    private const val NOTIFICATION_ID = 4711

    private const val ACTION_START = "dev.muxflow.ssh.action.START"
    const val ACTION_DISCONNECT = "dev.muxflow.ssh.action.DISCONNECT"
    private const val EXTRA_TITLE = "title"
    private const val EXTRA_BODY = "body"
    private const val WAKE_LOCK_TAG = "muxflow:ssh-connection"

    /** Set by [MuxflowSshModule] while it is alive; invoked by the notification's Disconnect action. */
    @Volatile var onDisconnectRequested: (() -> Unit)? = null

    fun start(context: Context, title: String, body: String) {
      val intent =
        Intent(context, ConnectionService::class.java).apply {
          action = ACTION_START
          putExtra(EXTRA_TITLE, title)
          putExtra(EXTRA_BODY, body)
        }
      ContextCompat.startForegroundService(context, intent)
    }

    fun stop(context: Context) {
      context.stopService(Intent(context, ConnectionService::class.java))
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
    val title =
      intent?.getStringExtra(EXTRA_TITLE) ?: applicationInfo.loadLabel(packageManager).toString()
    val body = intent?.getStringExtra(EXTRA_BODY).orEmpty()
    createChannel()
    startInForeground(buildNotification(title, body))
    acquireWakeLock()
    return START_NOT_STICKY
  }

  override fun onDestroy() {
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

  private fun createChannel() {
    val manager = getSystemService(NotificationManager::class.java) ?: return
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

  private fun buildNotification(title: String, body: String): Notification {
    val flags = PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    val disconnectIntent =
      PendingIntent.getService(
        this,
        1,
        Intent(this, ConnectionService::class.java).setAction(ACTION_DISCONNECT),
        flags,
      )
    val builder =
      NotificationCompat.Builder(this, CHANNEL_ID)
        .setSmallIcon(applicationInfo.icon)
        .setContentTitle(title)
        .setContentText(body)
        .setOngoing(true)
        .setSilent(true)
        .setShowWhen(false)
        .setCategory(NotificationCompat.CATEGORY_SERVICE)
        .setPriority(NotificationCompat.PRIORITY_LOW)
        .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
        .addAction(0, "Disconnect", disconnectIntent)
    packageManager.getLaunchIntentForPackage(packageName)?.let { launch ->
      builder.setContentIntent(PendingIntent.getActivity(this, 0, launch, flags))
    }
    return builder.build()
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
