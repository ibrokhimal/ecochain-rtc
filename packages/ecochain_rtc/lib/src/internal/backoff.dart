import 'dart:math';

import '../options.dart';

/// Computes reconnect delays for [RtcReconnectPolicy].
///
/// Kept free of timers and I/O so it can be unit tested.
class Backoff {
  /// Creates a backoff for [policy]. [random] is injectable for tests.
  Backoff(this.policy, {Random? random}) : _random = random ?? Random();

  /// The policy.
  final RtcReconnectPolicy policy;
  final Random _random;
  int _attempt = 0;

  /// Number of delays handed out so far.
  int get attempt => _attempt;

  /// Delay before the next attempt.
  Duration next() {
    final exponent = min(_attempt, 20);
    final base = policy.initialDelay.inMicroseconds * pow(2, exponent);
    final capped = min(base.toDouble(), policy.maxDelay.inMicroseconds.toDouble());
    _attempt++;
    final jitter = 0.5 + _random.nextDouble() * 0.5;
    return Duration(microseconds: (capped * jitter).round());
  }

  /// Starts over after a success.
  void reset() => _attempt = 0;
}
