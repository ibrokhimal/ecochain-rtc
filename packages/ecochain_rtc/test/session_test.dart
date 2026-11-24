import 'dart:async';
import 'dart:io';

import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/foundation.dart';
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

  group('video subscriptions', () {
    // Mobile platforms call the native speakerphone plugin after joining.
    setUp(() => debugDefaultTargetPlatformOverride = TargetPlatform.macOS);
    tearDown(() => debugDefaultTargetPlatformOverride = null);

    Map<String, dynamic> person(String id, List<String> videos) => {
      'participant_id': id,
      'identity': id,
      'name': id,
      'role': 'participant',
      'connected': true,
      'tracks': [
        for (final t in videos) {'track_id': t, 'source': 'camera', 'kind': 'video', 'muted': false, 'simulcast': false},
      ],
    };

    /// Track ids that exist on the server, and what it granted.
    late Set<String> published;
    late Set<String> granted;
    late int rejectNext;

    Future<RtcSession> join(List<Map<String, dynamic>> people) async {
      granted = {};
      rejectNext = 0;
      server.onRequest = (request, _) {
        final data = (request['data'] as Map?) ?? const {};
        switch (request['type']) {
          case 'join':
            // `resumed` skips the native peer connections.
            return FakeServer.ok(request, {
              'session_id': 'SE_1',
              'resumed': true,
              'participant': person('PA_me', []),
              'participants': people,
              'room': {'room_id': 'r', 'max_video_subscriptions': 9},
            });
          case 'subscription.update':
            if (rejectNext > 0) {
              rejectNext--;
              return FakeServer.error(request, 'bad_request');
            }
            final changes = (data['changes'] as List).cast<Map<String, dynamic>>();
            // Like the server: one unknown track rejects the whole batch.
            if (changes.any((c) => !published.contains(c['track_id']))) {
              return FakeServer.error(request, 'bad_request');
            }
            for (final c in changes) {
              c['subscribed'] == true ? granted.add(c['track_id'] as String) : granted.remove(c['track_id']);
            }
        }
        return FakeServer.ok(request, {});
      };
      final session = RtcSession();
      await session.connect(
        RtcConnectOptions(url: server.url, token: 't', allowInsecureLocalhost: true, microphone: false, camera: false),
      );
      await Future<void>.delayed(const Duration(milliseconds: 400));
      return session;
    }

    test('a participant who rejoins is subscribed again', () async {
      published = {'TR_a1', 'TR_b1'};
      final session = await join([person('PA_a', ['TR_a1']), person('PA_b', ['TR_b1'])]);
      expect(granted, {'TR_a1', 'TR_b1'});

      published = {'TR_a2', 'TR_b1'};
      server.push('participant.updated', {'participant': person('PA_a', [])});
      server.push('participant.updated', {'participant': person('PA_a', ['TR_a2'])});
      await Future<void>.delayed(const Duration(milliseconds: 400));
      expect(granted, contains('TR_a2'));
      session.dispose();
    });

    test('a newcomer is subscribed after somebody left', () async {
      published = {'TR_a1'};
      final session = await join([person('PA_a', ['TR_a1'])]);
      published = {};
      server.push('participant.left', {'participant_id': 'PA_a', 'reason': 'left'});
      await Future<void>.delayed(const Duration(milliseconds: 400));
      published = {'TR_c1'};
      server.push('participant.joined', {'participant': person('PA_c', ['TR_c1'])});
      await Future<void>.delayed(const Duration(milliseconds: 400));
      expect(granted, contains('TR_c1'));
      session.dispose();
    });

    test('a rejected update is retried without a further event', () async {
      published = {'TR_a1'};
      final session = await join([person('PA_a', ['TR_a1'])]);
      published = {'TR_a1', 'TR_b1'};
      rejectNext = 1;
      server.push('participant.joined', {'participant': person('PA_b', ['TR_b1'])});
      await Future<void>.delayed(const Duration(milliseconds: 2500));
      expect(granted, contains('TR_b1'));
      session.dispose();
    });
  });

  group('dead network', () {
    setUp(() => debugDefaultTargetPlatformOverride = TargetPlatform.macOS);
    tearDown(() => debugDefaultTargetPlatformOverride = null);

    test('a silent socket is replaced within seconds', () async {
      // The network changed: the old TCP connection stays "open" but nothing
      // gets through any more (no answers, no close handshake), while new
      // connections work.
      final relay = await _Relay.start(server.url.port);
      var joins = 0;
      server.onRequest = (request, _) {
        if (request['type'] == 'join') joins++;
        return FakeServer.ok(request, {
          'session_id': 'SE_1',
          'resumed': true,
          'participant': {'participant_id': 'PA_me', 'identity': 'me', 'name': 'me', 'tracks': []},
          'participants': [],
          'room': {'room_id': 'r', 'max_video_subscriptions': 9},
          'heartbeat_interval_ms': 5000,
        });
      };
      final session = RtcSession();
      await session.connect(
        RtcConnectOptions(
          url: relay.url,
          token: 't',
          allowInsecureLocalhost: true,
          microphone: false,
          camera: false,
        ),
      );
      relay.freezeExisting();
      final watch = Stopwatch()..start();
      while (joins < 2 && watch.elapsed < const Duration(seconds: 40)) {
        await Future<void>.delayed(const Duration(milliseconds: 50));
      }
      // ignore: avoid_print
      print('RESULT rejoin after ${watch.elapsedMilliseconds} ms');
      expect(joins, 2);
      expect(watch.elapsed, lessThan(const Duration(seconds: 7)));
      session.dispose();
      await relay.close();
    }, timeout: const Timeout(Duration(seconds: 60)));
  });
}

/// TCP relay whose existing connections can be frozen.
class _Relay {
  _Relay._(this._server);

  final ServerSocket _server;
  final _frozen = <Socket>{};
  final _open = <Socket>{};

  Uri get url => Uri.parse('ws://127.0.0.1:${_server.port}/v1/rtc');

  static Future<_Relay> start(int target) async {
    final relay = _Relay._(await ServerSocket.bind(InternetAddress.loopbackIPv4, 0));
    relay._server.listen((client) async {
      final upstream = await Socket.connect(InternetAddress.loopbackIPv4, target);
      relay._open.addAll([client, upstream]);
      void pipe(Socket from, Socket to) => from.listen(
        (data) {
          if (!relay._frozen.contains(from)) to.add(data);
        },
        onDone: () {
          if (!relay._frozen.contains(from)) to.destroy();
        },
        onError: (Object _) {},
      );
      pipe(client, upstream);
      pipe(upstream, client);
    });
    return relay;
  }

  void freezeExisting() => _frozen.addAll(_open);

  Future<void> close() async {
    for (final socket in _open) {
      socket.destroy();
    }
    await _server.close();
  }
}
