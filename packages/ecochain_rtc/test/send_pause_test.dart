import 'dart:async';

import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:ecochain_rtc/src/internal/interruptions.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_server.dart';

Future<void> settle() => Future<void>.delayed(const Duration(milliseconds: 150));

void main() {
  test('a reason starts at once and ends only in the foreground', () {
    final reasons = PauseReasons()..report(locked: false, call: true);
    expect(reasons.current, {RtcSendPauseReason.phoneCall});
    reasons
      ..setForeground(false)
      ..report(locked: true, call: false);
    expect(reasons.current, RtcSendPauseReason.values.toSet());
    reasons.report(locked: false, call: false);
    expect(reasons.current, isNotEmpty, reason: 'still in the background');
    reasons.setForeground(true);
    expect(reasons.current, isEmpty);
  });

  test('a ringing call that is declined in the foreground ends the pause', () {
    final reasons = PauseReasons()..report(locked: false, call: true);
    reasons.report(locked: false, call: false);
    expect(reasons.current, isEmpty);
  });

  group('session', () {
    late FakeServer server;
    late StreamController<Set<RtcSendPauseReason>> signals;
    late RtcSession session;
    var tracks = 0;

    setUp(() async {
      TestWidgetsFlutterBinding.ensureInitialized();
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      server = await FakeServer.start();
      server.onRequest = (request, _) => FakeServer.ok(request, {
        if (request['type'] == 'join') ...{
          'session_id': 'SE_1',
          'resumed': true,
          'participant': {
            'participant_id': 'PA_1',
            'identity': 'me',
            'name': 'Me',
            'role': 'participant',
            'tracks': [],
          },
          'participants': <Object>[],
          'room': {'room_id': 'r', 'max_video_subscriptions': 9},
        },
      });
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('FlutterWebRTC.Method'),
        (call) async {
          if (call.method != 'getUserMedia') return <String, dynamic>{};
          final constraints = (call.arguments as Map)['constraints'] as Map;
          final kind = constraints['video'] == false ? 'audio' : 'video';
          final track = {'id': 't${++tracks}', 'label': kind, 'kind': kind, 'enabled': true, 'settings': {}};
          return {
            'streamId': 's$tracks',
            'audioTracks': [if (kind == 'audio') track],
            'videoTracks': [if (kind == 'video') track],
          };
        },
      );
      signals = StreamController.broadcast(sync: true);
      session = RtcSession(interruptions: () => signals.stream);
      await session.connect(
        RtcConnectOptions(url: server.url, token: 't', allowInsecureLocalhost: true, microphone: false, camera: false),
      );
      await session.setMicrophoneEnabled(true).catchError((_) {});
      await session.setCameraEnabled(true).catchError((_) {});
      await settle();
    });

    tearDown(() async {
      await session.leave();
      session.dispose();
      await server.close();
      debugDefaultTargetPlatformOverride = null;
    });

    test('releases devices during the pause and restores the choice after', () async {
      expect((session.microphoneEnabled, session.cameraEnabled), (true, true));
      final media = [for (final t in session.localParticipant!.tracks) t.mediaTrack!];
      signals.add({RtcSendPauseReason.phoneCall});
      expect(media.every((t) => !t.enabled), isTrue, reason: 'silenced synchronously');
      await settle();
      expect(session.localParticipant!.tracks, isEmpty);
      expect((session.microphoneEnabled, session.cameraEnabled), (false, false));

      // Nothing can be turned on meanwhile, but the choice is remembered.
      await session.setCameraEnabled(false);
      await session.setMicrophoneEnabled(true);
      signals.add({RtcSendPauseReason.phoneCall, RtcSendPauseReason.screenLocked});
      await settle();
      expect(session.localParticipant!.tracks, isEmpty);

      signals.add({});
      await settle();
      expect((session.microphoneEnabled, session.cameraEnabled), (true, false));
    });

    test('a pause that ends right away leaves the tracks sending', () async {
      signals
        ..add({RtcSendPauseReason.screenLocked})
        ..add({});
      await settle();
      expect(session.localParticipant!.tracks.length, 2);
      expect(session.localParticipant!.tracks.every((t) => t.mediaTrack!.enabled), isTrue);
    });
  });
}
