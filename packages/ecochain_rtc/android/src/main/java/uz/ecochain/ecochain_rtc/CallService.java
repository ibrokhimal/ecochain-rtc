package uz.ecochain.ecochain_rtc;

import android.Manifest;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Person;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.graphics.drawable.Icon;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;
import android.util.Log;
import java.util.Map;

/**
 * Keeps the meeting alive in the background and shows it as an ongoing call. Main thread only.
 */
public class CallService extends Service {
  /** Receives the notification's buttons. */
  interface Listener {
    void onMute(boolean muted);

    void onHangUp();
  }

  private static final String TAG = "ecochain_rtc";
  private static final String CHANNEL = "ecochain_rtc_call";
  private static final int ID = 0x0ec0;
  private static final String ACTION_MUTE = "uz.ecochain.ecochain_rtc.MUTE";
  private static final String ACTION_HANG_UP = "uz.ecochain.ecochain_rtc.HANG_UP";

  static Listener listener;
  private static Map<String, Object> state;
  private static CallService instance;
  private static long startedAt;

  private PowerManager.WakeLock wakeLock;
  private int types;

  static void show(Context context, Map<String, Object> newState) {
    boolean first = state == null;
    state = newState;
    if (first) {
      startedAt = System.currentTimeMillis();
      try {
        context.startForegroundService(new Intent(context, CallService.class));
      } catch (RuntimeException e) {
        // Not allowed from the background; the meeting goes on without it.
        Log.w(TAG, "call service not started", e);
        state = null;
      }
    } else if (instance != null) {
      instance.refresh();
    }
  }

  static void end() {
    state = null;
    // Not created yet: onStartCommand sees the missing state and stops.
    if (instance != null) instance.stop();
  }

  /** Shows the notification after the permission was granted. */
  static void repost() {
    if (instance != null && state != null) instance.refresh();
  }

  @Override
  public void onCreate() {
    instance = this;
    NotificationChannel channel =
        new NotificationChannel(
            CHANNEL, getApplicationInfo().loadLabel(getPackageManager()), NotificationManager.IMPORTANCE_DEFAULT);
    channel.setSound(null, null);
    channel.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
    manager().createNotificationChannel(channel);
    wakeLock =
        ((PowerManager) getSystemService(POWER_SERVICE))
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ecochain_rtc:call");
    wakeLock.setReferenceCounted(false);
  }

  @Override
  public int onStartCommand(Intent intent, int flags, int startId) {
    String action = intent == null ? null : intent.getAction();
    if (action == null) {
      // Promised to the system by startForegroundService, even if the
      // meeting has ended meanwhile.
      foreground();
      if (state == null) stop();
      else wakeLock.acquire();
    } else if (listener == null || state == null) {
      stop();
    } else if (ACTION_HANG_UP.equals(action)) {
      listener.onHangUp();
    } else if (ACTION_MUTE.equals(action)) {
      listener.onMute(!is("muted"));
    }
    return START_NOT_STICKY;
  }

  @Override
  public void onTaskRemoved(Intent rootIntent) {
    // The Flutter engine, and the meeting with it, goes with the task.
    if (listener != null) listener.onHangUp();
    end();
  }

  @Override
  public void onDestroy() {
    wakeLock.release();
    instance = null;
  }

  @Override
  public IBinder onBind(Intent intent) {
    return null;
  }

  private void stop() {
    stopForeground(STOP_FOREGROUND_REMOVE);
    stopSelf();
  }

  private void refresh() {
    // The microphone permission may have been granted since.
    if (types != wantedTypes()) foreground();
    manager().notify(ID, notification());
  }

  private int wantedTypes() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return 0;
    boolean microphone = checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED;
    return ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
        | (microphone ? ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE : 0);
  }

  private void foreground() {
    int wanted = wantedTypes();
    try {
      start(wanted);
    } catch (RuntimeException e) {
      // The microphone may only be claimed while the app is in front.
      Log.w(TAG, "call service without microphone", e);
      if (types == 0 && wanted != 0) start(ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
    }
  }

  private void start(int wanted) {
    if (wanted == 0) startForeground(ID, notification());
    else startForeground(ID, notification(), wanted);
    types = wanted;
  }

  private Notification notification() {
    Intent open = getPackageManager().getLaunchIntentForPackage(getPackageName());
    PendingIntent hangUp = action(ACTION_HANG_UP, 1);
    Notification.Builder builder =
        new Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ecochain_rtc_call)
            .setContentTitle(text("title"))
            .setContentText(text(is("paused") ? "pausedText" : "ongoing"))
            .setCategory(Notification.CATEGORY_CALL)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setWhen(startedAt)
            .setShowWhen(true)
            .setUsesChronometer(true);
    if (open != null) {
      builder.setContentIntent(PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE));
    }
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
      String title = text("title");
      Person person = new Person.Builder().setName(title.isEmpty() ? " " : title).setImportant(true).build();
      builder.setStyle(Notification.CallStyle.forOngoingCall(person, hangUp));
      builder.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE);
    } else {
      builder.addAction(button(text("hangUpLabel"), hangUp));
    }
    // While sending is paused the microphone cannot be turned on.
    if (!is("paused")) {
      builder.addAction(button(text(is("muted") ? "unmuteLabel" : "muteLabel"), action(ACTION_MUTE, 2)));
    }
    return builder.build();
  }

  private Notification.Action button(String label, PendingIntent intent) {
    return new Notification.Action.Builder(Icon.createWithResource(this, R.drawable.ecochain_rtc_call), label, intent)
        .build();
  }

  private PendingIntent action(String name, int code) {
    Intent intent = new Intent(this, CallService.class).setAction(name);
    return PendingIntent.getService(this, code, intent, PendingIntent.FLAG_IMMUTABLE);
  }

  private NotificationManager manager() {
    return (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
  }

  private static boolean is(String key) {
    return state != null && Boolean.TRUE.equals(state.get(key));
  }

  private static String text(String key) {
    Object value = state == null ? null : state.get(key);
    return value == null ? "" : value.toString();
  }
}
