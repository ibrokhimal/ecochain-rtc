package uz.ecochain.ecochain_rtc;

import android.Manifest;
import android.app.Activity;
import android.app.KeyguardManager;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.PackageManager;
import android.media.AudioManager;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.PowerManager;
import androidx.annotation.NonNull;
import io.flutter.embedding.engine.plugins.FlutterPlugin;
import io.flutter.embedding.engine.plugins.activity.ActivityAware;
import io.flutter.embedding.engine.plugins.activity.ActivityPluginBinding;
import io.flutter.plugin.common.EventChannel;
import io.flutter.plugin.common.MethodCall;
import io.flutter.plugin.common.MethodChannel;
import io.flutter.plugin.common.PluginRegistry;
import java.util.HashMap;
import java.util.Map;

/**
 * Shows the meeting as a call ({@link CallService}) and reports {@code {"locked": bool, "call": bool}} while somebody listens. Needs no permission: calls
 * are recognised by the system audio mode (ringing or in a phone call).
 */
public class EcochainRtcPlugin
    implements FlutterPlugin,
        ActivityAware,
        EventChannel.StreamHandler,
        MethodChannel.MethodCallHandler,
        CallService.Listener {
  private static final int NOTIFICATION_REQUEST = 0x0ec0;

  private static final long POLL_MS = 500;

  private final Handler handler = new Handler(Looper.getMainLooper());
  private Context context;
  private EventChannel channel;
  private MethodChannel callChannel;
  private ActivityPluginBinding activity;
  private EventChannel.EventSink sink;
  private boolean locked;
  private boolean call;

  private final BroadcastReceiver receiver =
      new BroadcastReceiver() {
        @Override
        public void onReceive(Context c, Intent intent) {
          update(Intent.ACTION_SCREEN_OFF.equals(intent.getAction()) || isLocked(), isCall());
        }
      };

  // The mode listener only exists since API 31, so the mode is polled; the
  // keyguard too, because some devices skip USER_PRESENT.
  private final Runnable poll =
      new Runnable() {
        @Override
        public void run() {
          update(isLocked(), isCall());
          handler.postDelayed(this, POLL_MS);
        }
      };

  @Override
  public void onAttachedToEngine(@NonNull FlutterPluginBinding binding) {
    context = binding.getApplicationContext();
    channel = new EventChannel(binding.getBinaryMessenger(), "ecochain_rtc/interruptions");
    channel.setStreamHandler(this);
    callChannel = new MethodChannel(binding.getBinaryMessenger(), "ecochain_rtc/call");
    callChannel.setMethodCallHandler(this);
  }

  @Override
  public void onDetachedFromEngine(@NonNull FlutterPluginBinding binding) {
    stop();
    channel.setStreamHandler(null);
    callChannel.setMethodCallHandler(null);
    if (CallService.listener == this) {
      CallService.listener = null;
      CallService.end();
    }
  }

  @Override
  public void onMethodCall(@NonNull MethodCall call, @NonNull MethodChannel.Result result) {
    switch (call.method) {
      case "show":
        CallService.listener = this;
        CallService.show(context, call.arguments());
        break;
      case "end":
        if (CallService.listener == this) CallService.end();
        break;
      case "requestPermission":
        requestNotificationPermission();
        break;
      default:
        result.notImplemented();
        return;
    }
    result.success(null);
  }

  @Override
  public void onMute(boolean muted) {
    callChannel.invokeMethod("mute", muted);
  }

  @Override
  public void onHangUp() {
    callChannel.invokeMethod("hangUp", null);
  }

  private void requestNotificationPermission() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU || activity == null) return;
    Activity a = activity.getActivity();
    String permission = Manifest.permission.POST_NOTIFICATIONS;
    if (a.checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED) {
      a.requestPermissions(new String[] {permission}, NOTIFICATION_REQUEST);
    }
  }

  private final PluginRegistry.RequestPermissionsResultListener permissionResult =
      (code, permissions, results) -> {
        if (code != NOTIFICATION_REQUEST) return false;
        CallService.repost();
        return true;
      };

  @Override
  public void onAttachedToActivity(@NonNull ActivityPluginBinding binding) {
    activity = binding;
    binding.addRequestPermissionsResultListener(permissionResult);
  }

  @Override
  public void onDetachedFromActivity() {
    if (activity != null) activity.removeRequestPermissionsResultListener(permissionResult);
    activity = null;
  }

  @Override
  public void onReattachedToActivityForConfigChanges(@NonNull ActivityPluginBinding binding) {
    onAttachedToActivity(binding);
  }

  @Override
  public void onDetachedFromActivityForConfigChanges() {
    onDetachedFromActivity();
  }

  @Override
  public void onListen(Object arguments, EventChannel.EventSink events) {
    stop();
    sink = events;
    locked = isLocked();
    call = isCall();
    IntentFilter filter = new IntentFilter(Intent.ACTION_SCREEN_OFF);
    filter.addAction(Intent.ACTION_SCREEN_ON);
    filter.addAction(Intent.ACTION_USER_PRESENT);
    context.registerReceiver(receiver, filter);
    send();
    handler.postDelayed(poll, POLL_MS);
  }

  @Override
  public void onCancel(Object arguments) {
    stop();
  }

  private void stop() {
    if (sink == null) return;
    sink = null;
    handler.removeCallbacks(poll);
    context.unregisterReceiver(receiver);
  }

  private void update(boolean nowLocked, boolean nowCall) {
    if (locked == nowLocked && call == nowCall) return;
    locked = nowLocked;
    call = nowCall;
    send();
  }

  private void send() {
    if (sink == null) return;
    Map<String, Object> event = new HashMap<>();
    event.put("locked", locked);
    event.put("call", call);
    sink.success(event);
  }

  private boolean isLocked() {
    PowerManager power = (PowerManager) context.getSystemService(Context.POWER_SERVICE);
    KeyguardManager keyguard = (KeyguardManager) context.getSystemService(Context.KEYGUARD_SERVICE);
    return !power.isInteractive() || keyguard.isKeyguardLocked();
  }

  private boolean isCall() {
    int mode = ((AudioManager) context.getSystemService(Context.AUDIO_SERVICE)).getMode();
    return mode == AudioManager.MODE_RINGTONE
        || mode == AudioManager.MODE_IN_CALL
        || mode == AudioManager.MODE_CALL_SCREENING;
  }
}
