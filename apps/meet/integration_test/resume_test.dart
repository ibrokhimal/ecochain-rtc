// Regression test: remote audio must arrive after the UI thread froze
// (macOS permission dialog) and the session resumed.
//
// Run against a local development server:
//   cd server && ./target/release/ecochain-rtc-server serve --config config/development.toml
//   cd apps/meet && flutter test integration_test/resume_test.dart -d macos
//
// A str0m bot (`loadtest`) acts as the remote participant; start it when the
// test prints "START BOT". Progress goes to stdout because the test
// runner shows it while the app's stderr is not forwarded.
// ignore_for_file: avoid_print
import 'dart:io';

import 'package:ecochain_meeting_ui/ecochain_meeting_ui.dart';
import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

const _url = String.fromEnvironment('ECOCHAIN_RTC_URL', defaultValue: 'ws://127.0.0.1:7880/v1/rtc');
const _room = String.fromEnvironment('ECOCHAIN_ROOM', defaultValue: 'resume-test');
const _freezeSeconds = int.fromEnvironment('FREEZE_SECONDS', defaultValue: 20);

Future<int> audioBytesAfter(RtcSession session, Duration wait) async {
  final before = (await session.getReceiveStats()).audioBytes;
  await Future<void>.delayed(wait);
  return (await session.getReceiveStats()).audioBytes - before;
}

Future<void> waitFor(bool Function() condition, Duration timeout, String what) async {
  final end = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(end)) fail('timeout waiting for $what');
    await Future<void>.delayed(const Duration(milliseconds: 200));
  }
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('audio arrives after the UI thread was frozen and the session resumed', (tester) async {
    final token = await fetchDevToken(Uri.parse(_url), room: _room, identity: 'alice-it', name: 'Alice IT');
    final session = RtcSession();
    final states = <RtcConnectionState>[];
    session.addListener(() {
      if (states.isEmpty || states.last != session.connectionState) states.add(session.connectionState);
    });
    await session.connect(RtcConnectOptions(url: Uri.parse(_url), token: token, allowInsecureLocalhost: true));

    // Freeze the isolate like a modal permission dialog does on macOS
    // (merged UI/platform thread): no timers, no socket reads.
    print('FREEZE ${_freezeSeconds}s');
    sleep(const Duration(seconds: _freezeSeconds));
    print('UNFROZEN');

    await waitFor(
      () => session.connectionState == RtcConnectionState.connected && states.contains(RtcConnectionState.reconnecting),
      const Duration(seconds: 30),
      'resume',
    );
    print('RESUMED states=$states');

    print('START BOT');
    await waitFor(
      () => session.remoteParticipants.any((p) => p.microphoneOn),
      const Duration(seconds: 60),
      'bot audio track',
    );
    await Future<void>.delayed(const Duration(seconds: 3));
    final bytes = await audioBytesAfter(session, const Duration(seconds: 4));
    print('AUDIO BYTES in 4s: $bytes  stats=${await session.getReceiveStats()}');
    expect(bytes, greaterThan(5000), reason: 'remote audio must flow after resume');
    await session.leave();
    session.dispose();
  }, timeout: const Timeout(Duration(minutes: 3)));
}
