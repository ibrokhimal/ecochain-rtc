import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:wakelock_plus/wakelock_plus.dart';

/// Keeps the screen from dimming and auto-locking while anybody holds it.
///
/// Counted, because a new meeting screen can open before the old one is
/// disposed.
class KeepAwake {
  static int _holders = 0;
  bool _held = false;

  /// Holds or releases the screen.
  void set(bool on) {
    if (on == _held) return;
    _held = on;
    _holders += on ? 1 : -1;
    if (_holders == (on ? 1 : 0)) unawaited(_apply(on));
  }

  static Future<void> _apply(bool on) async {
    try {
      await WakelockPlus.toggle(enable: on);
    } catch (e) {
      // A screen that goes dark must not break the call.
      debugPrint('ecochain_meeting_ui: keep awake: $e');
    }
  }
}
