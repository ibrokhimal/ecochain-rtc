import 'dart:async';

import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:ecochain_rtc/src/internal/video_debug_overlay.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';

RtcTrack track(String id, Future<List<StatsReport>> Function() reader, {bool local = false}) =>
    RtcTrack(id: id, source: RtcTrackSource.camera, muted: false, simulcast: true, isLocal: local, readStats: reader);

List<StatsReport> stats(int bytes, int frames) => [
  StatsReport('c', 'codec', 0, {'mimeType': 'video/VP8'}),
  StatsReport('v', 'inbound-rtp', 0, {
    'kind': 'video',
    'codecId': 'c',
    'frameWidth': 640,
    'frameHeight': 360,
    'bytesReceived': bytes,
    'framesDecoded': frames,
  }),
];

Widget overlay(RtcTrack track, {Duration Function()? clock}) => Directionality(
  textDirection: TextDirection.ltr,
  child: VideoDebugOverlay(track: track, clock: clock),
);

void main() {
  testWidgets('shows real per-track video statistics above the picture', (tester) async {
    var reads = 0;
    var now = Duration.zero;
    final remote = track('remote', () async => stats(1000 + reads * 60000, 10 + (reads++) * 25));
    await tester.pumpWidget(overlay(remote, clock: () => now));
    await tester.pump();
    expect(find.textContaining('RX • VP8 • 640×360'), findsOneWidget);
    now = const Duration(seconds: 1);
    await tester.pump(const Duration(seconds: 1));
    await tester.pump();
    expect(find.textContaining('25 FPS • 480 kbit/s'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    await tester.pump(const Duration(seconds: 2));
    expect(reads, 2, reason: 'disposing the tile must cancel polling');
  });

  testWidgets('local preview is labelled without receive-stat polling', (tester) async {
    var reads = 0;
    await tester.pumpWidget(
      overlay(
        track('local', () async {
          reads++;
          return [];
        }, local: true),
      ),
    );
    await tester.pump(const Duration(seconds: 3));
    expect(find.text('LOCAL'), findsOneWidget);
    expect(reads, 0);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('does not overlap reads or display a late result from the previous track', (tester) async {
    final late = Completer<List<StatsReport>>();
    var reads = 0;
    await tester.pumpWidget(
      overlay(
        track('old', () {
          reads++;
          return late.future;
        }),
      ),
    );
    await tester.pump(const Duration(seconds: 5));
    expect(reads, 1, reason: 'even a slow stats request must not overlap another one');
    await tester.pumpWidget(overlay(track('new', () async => [])));
    await tester.pump();
    late.complete(stats(1000, 10));
    await tester.pump();
    expect(find.textContaining('VP8'), findsNothing);
    expect(find.textContaining('— FPS • — kbit/s'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('stats errors stay inside the debug overlay', (tester) async {
    await tester.pumpWidget(overlay(track('remote', () async => throw StateError('peer closed'))));
    await tester.pump();
    expect(find.textContaining('RX • — • —'), findsOneWidget);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
  });
}
