import 'dart:async';

import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_server.dart';

RtcConnectOptions options(Uri url) => RtcConnectOptions(
  url: url,
  token: 'token',
  allowInsecureLocalhost: true,
  requestTimeout: const Duration(seconds: 2),
);

void main() {
  late FakeServer server;
  setUp(() async => server = await FakeServer.start());
  tearDown(() => server.close());

  test('rejects insecure and malformed URLs before connecting', () {
    final session = RtcSession();
    expect(
      () => session.connect(RtcConnectOptions(url: Uri.parse('ws://meet.example.com/v1/rtc'), token: 't')),
      throwsA(isA<RtcException>().having((e) => e.code, 'code', RtcErrorCode.invalidUrl)),
    );
    expect(
      () => session.connect(
        RtcConnectOptions(url: Uri.parse('ws://meet.example.com/v1/rtc'), token: 't', allowInsecureLocalhost: true),
      ),
      throwsA(isA<RtcException>()),
      reason: 'ws:// is only for loopback and private LAN hosts',
    );
    expect(session.connectionState, RtcConnectionState.idle);
    session.dispose();
  });

  test('a rejected join ends the session with the right reason', () async {
    server.onRequest = (request, _) => FakeServer.error(request, 'room_full');
    final session = RtcSession();
    final states = <RtcConnectionState>[];
    session.addListener(() => states.add(session.connectionState));
    await expectLater(session.connect(options(server.url)), throwsA(isA<RtcException>()));
    expect(session.connectionState, RtcConnectionState.disconnected);
    expect(session.disconnectReason, RtcDisconnectReason.roomFull);
    expect(states, containsAllInOrder([RtcConnectionState.connecting, RtcConnectionState.disconnected]));
    session.dispose();
  });

  test('concurrent connect calls share one attempt', () async {
    final joins = Completer<void>();
    var joinCount = 0;
    server.onRequest = (request, _) async {
      if (request['type'] == 'join') {
        joinCount++;
        await joins.future;
        return FakeServer.error(request, 'unauthorized');
      }
      return FakeServer.ok(request, {});
    };
    final session = RtcSession();
    final first = session.connect(options(server.url));
    final second = session.connect(options(server.url));
    expect(identical(first, second), isTrue);
    joins.complete();
    await expectLater(first, throwsA(isA<RtcException>()));
    expect(joinCount, 1);
    expect(server.sockets.length, 1, reason: 'no second socket was opened');
    session.dispose();
  });

  test('leave during connect discards the late join result', () async {
    final release = Completer<void>();
    server.onRequest = (request, _) async {
      if (request['type'] != 'join') return FakeServer.ok(request, {});
      await release.future;
      return FakeServer.ok(request, {
        'session_id': 'SE_1',
        'resumed': false,
        'participant': {'participant_id': 'PA_1', 'identity': 'me', 'name': 'Me', 'role': 'participant', 'tracks': []},
        'participants': [],
        'room': {'room_id': 'r', 'max_video_subscriptions': 9},
      });
    };
    final session = RtcSession();
    final connecting = session.connect(options(server.url));
    await Future<void>.delayed(const Duration(milliseconds: 100));
    await session.leave();
    expect(session.connectionState, RtcConnectionState.disconnected);
    expect(session.disconnectReason, RtcDisconnectReason.left);
    release.complete();
    await connecting;
    await Future<void>.delayed(const Duration(milliseconds: 100));
    // The late response must not revive the session or keep a socket.
    expect(session.connectionState, RtcConnectionState.disconnected);
    expect(session.localParticipant, isNull);
    await Future<void>.delayed(const Duration(milliseconds: 100));
    expect(server.sockets.single.closeCode, isNotNull, reason: 'socket was closed');
    session.dispose();
  });

  test('device toggles require an active session', () async {
    final session = RtcSession();
    await expectLater(
      session.setMicrophoneEnabled(true),
      throwsA(isA<RtcException>().having((e) => e.code, 'code', RtcErrorCode.notConnected)),
    );
    expect(session.microphoneEnabled, isFalse);
    session.dispose();
  });

  test('unreachable server ends with networkFailed', () async {
    final url = server.url;
    await server.close();
    final session = RtcSession();
    await expectLater(session.connect(options(url)), throwsA(isA<RtcException>()));
    expect(session.disconnectReason, RtcDisconnectReason.networkFailed);
    session.dispose();
  });
}
