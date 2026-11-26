import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../options.dart';

/// What the user did on the system's call screen or notification.
typedef SystemCallActions = ({
  void Function(bool muted) onMute,
  void Function(bool held) onHold,
  void Function() onHangUp,
});

/// Shows the running meeting as a call of the operating system: CallKit on
/// iOS, a foreground service with a call notification on Android.
class SystemCall {
  static const _channel = MethodChannel('ecochain_rtc/call');

  /// The channel is shared; the session that shows a call owns it.
  static SystemCall? _owner;

  Map<String, Object>? _shown;

  bool get _supported => !kIsWeb && (Platform.isAndroid || Platform.isIOS);

  /// Starts the call, or brings it up to date.
  void show(
    RtcSystemCall call, {
    required bool connected,
    required bool muted,
    required bool paused,
    required SystemCallActions actions,
  }) {
    if (!_supported) return;
    final state = <String, Object>{
      'title': call.title,
      'ongoing': call.ongoingText,
      'pausedText': call.pausedText,
      'muteLabel': call.muteLabel,
      'unmuteLabel': call.unmuteLabel,
      'hangUpLabel': call.hangUpLabel,
      'connected': connected,
      'muted': muted,
      'paused': paused,
    };
    if (mapEquals(state, _shown)) return;
    if (_owner != this) {
      _owner = this;
      _channel.setMethodCallHandler((event) async {
        switch (event.method) {
          case 'mute':
            actions.onMute(event.arguments as bool);
            // The system already shows the new state; tell it if that is
            // not what happened.
            _shown = null;
          case 'hold':
            actions.onHold(event.arguments as bool);
          case 'hangUp':
            actions.onHangUp();
        }
      });
    }
    _shown = state;
    _invoke('show', state);
  }

  /// Ends the call.
  void end() {
    if (_owner != this) return;
    _owner = null;
    _shown = null;
    _channel.setMethodCallHandler(null);
    _invoke('end');
  }

  /// Asks for the Android 13+ notification permission. Without it the call
  /// keeps running, only its notification is hidden.
  void requestPermission() {
    if (_owner == this && !kIsWeb && Platform.isAndroid) _invoke('requestPermission');
  }

  static void _invoke(String method, [Object? arguments]) {
    unawaited(
      _channel.invokeMethod<void>(method, arguments).catchError((Object e) {
        // The meeting works without the system's call screen.
        debugPrint('ecochain_rtc: system call: $e');
      }),
    );
  }
}
