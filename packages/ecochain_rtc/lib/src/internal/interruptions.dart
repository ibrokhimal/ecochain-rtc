import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../connection_state.dart';

/// Reasons to stop sending, from what the OS reports.
///
/// A reason starts at once but only ends in the foreground: the OS does not
/// let a background app open the camera or microphone again.
class PauseReasons {
  final _raw = <RtcSendPauseReason>{};
  bool _foreground = true;

  /// Reasons in force.
  Set<RtcSendPauseReason> get current => Set.unmodifiable(_current);
  final _current = <RtcSendPauseReason>{};

  /// What the OS says now.
  void report({required bool locked, required bool call}) {
    _raw.clear();
    if (locked) _raw.add(RtcSendPauseReason.screenLocked);
    if (call) _raw.add(RtcSendPauseReason.phoneCall);
    _update();
  }

  /// Whether the app is in front.
  void setForeground(bool value) {
    _foreground = value;
    _update();
  }

  void _update() {
    _current.addAll(_raw);
    if (_foreground) _current.retainAll(_raw);
  }
}

const _channel = EventChannel('ecochain_rtc/interruptions');
final Stream<dynamic> _native = _channel.receiveBroadcastStream();

/// Screen lock and phone calls on Android and iOS; silent elsewhere.
Stream<Set<RtcSendPauseReason>> platformInterruptions() {
  if (kIsWeb || !(Platform.isAndroid || Platform.isIOS)) return const Stream.empty();
  final reasons = PauseReasons();
  late final StreamController<Set<RtcSendPauseReason>> controller;
  AppLifecycleListener? lifecycle;
  StreamSubscription<dynamic>? events;
  controller = StreamController(
    sync: true,
    onListen: () {
      final state = WidgetsBinding.instance.lifecycleState;
      reasons.setForeground(state == null || state == AppLifecycleState.resumed);
      lifecycle = AppLifecycleListener(
        onStateChange: (state) {
          reasons.setForeground(state == AppLifecycleState.resumed);
          controller.add(reasons.current);
        },
      );
      events = _native.listen((event) {
        final map = event as Map;
        reasons.report(locked: map['locked'] == true, call: map['call'] == true);
        controller.add(reasons.current);
      }, onError: (Object e) => debugPrint('ecochain_rtc: interruptions: $e'));
    },
    onCancel: () {
      lifecycle?.dispose();
      return events?.cancel();
    },
  );
  return controller.stream;
}
