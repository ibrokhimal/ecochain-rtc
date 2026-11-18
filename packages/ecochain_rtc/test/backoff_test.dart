import 'dart:math';

import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:ecochain_rtc/src/internal/backoff.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('delays grow exponentially, are capped and jittered', () {
    const policy = RtcReconnectPolicy(initialDelay: Duration(milliseconds: 100), maxDelay: Duration(seconds: 1));
    final backoff = Backoff(policy, random: Random(1));
    final delays = [for (var i = 0; i < 8; i++) backoff.next().inMilliseconds];
    final bases = [100, 200, 400, 800, 1000, 1000, 1000, 1000];
    for (var i = 0; i < delays.length; i++) {
      expect(delays[i], inInclusiveRange(bases[i] ~/ 2, bases[i]), reason: 'attempt $i: ${delays[i]}');
    }
    backoff.reset();
    expect(backoff.next().inMilliseconds, lessThanOrEqualTo(100));
  });

  test('jitter spreads clients apart', () {
    const policy = RtcReconnectPolicy();
    final first = {for (var seed = 0; seed < 20; seed++) Backoff(policy, random: Random(seed)).next()};
    expect(first.length, greaterThan(10));
  });
}
