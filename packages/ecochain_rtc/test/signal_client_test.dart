import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:ecochain_rtc/src/internal/signal_client.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_server.dart';

void main() {
  late FakeServer server;
  setUp(() async => server = await FakeServer.start());
  tearDown(() => server.close());

  test('matches responses to requests and maps error codes', () async {
    server.onRequest = (request, _) => switch (request['type']) {
      'join' => FakeServer.error(request, 'room_full'),
      _ => FakeServer.ok(request, {'echo': request['type']}),
    };
    final client = await SignalClient.connect(server.url, const Duration(seconds: 2));
    expect(await client.request('ping'), {'echo': 'ping'});
    await expectLater(
      client.request('join', {'token': 't'}),
      throwsA(isA<RtcException>().having((e) => e.code, 'code', RtcErrorCode.roomFull)),
    );
    expect(server.received.first['v'], 1, reason: 'protocol version is sent');
    await client.close();
  });

  test('times out and fails pending requests when the socket closes', () async {
    server.onRequest = (request, _) => null;
    final client = await SignalClient.connect(server.url, const Duration(milliseconds: 200));
    await expectLater(
      client.request('ping'),
      throwsA(isA<RtcException>().having((e) => e.code, 'code', RtcErrorCode.timeout)),
    );
    final pending = client.request('ping');
    await server.sockets.single.close(4004, 'room ended');
    await expectLater(pending, throwsA(isA<RtcException>()));
    expect(await client.closed, 4004);
  });

  test('delivers push events', () async {
    final client = await SignalClient.connect(server.url, const Duration(seconds: 2));
    final event = client.events.first;
    await Future<void>.delayed(const Duration(milliseconds: 50));
    server.push('speakers.changed', {'speakers': []});
    expect((await event).type, 'speakers.changed');
    await client.close();
  });

  test('heartbeat closes a silent socket', () async {
    server.onRequest = (request, _) => null; // never answers pings
    final client = await SignalClient.connect(server.url, const Duration(seconds: 5));
    client.startHeartbeat(const Duration(milliseconds: 50), const Duration(milliseconds: 200));
    final code = await client.closed.timeout(const Duration(seconds: 2));
    expect(code, isNull);
    expect(client.isClosed, isTrue);
  });
}
