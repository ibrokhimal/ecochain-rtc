import 'package:ecochain_meeting_ui/ecochain_meeting_ui.dart';
import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

class _FakeSession extends RtcSession {
  _FakeSession(this.count);
  final int count;

  @override
  RtcConnectionState get connectionState => RtcConnectionState.connected;

  @override
  List<RtcParticipant> get participants => [
    for (var i = 0; i < count; i++)
      RtcParticipant(
        id: 'p$i',
        identity: 'p$i',
        name: 'User $i',
        role: RtcRole.participant,
        isLocal: i == 0,
        connected: true,
        tracks: const [],
      ),
  ];

  @override
  void setVideoLayout({required List<String> visibleParticipantIds, String? pinnedParticipantId}) {}
}

void main() {
  for (final size in const [Size(390, 844), Size(844, 390), Size(1280, 800)]) {
    for (final count in [1, 2, 3, 8, 30]) {
      testWidgets('$count tiles fit at $size, also when pinned', (tester) async {
        tester.view.physicalSize = size;
        tester.view.devicePixelRatio = 1;
        addTearDown(tester.view.reset);
        tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          const MethodChannel('FlutterWebRTC.Method'),
          (call) async => call.method == 'createVideoRenderer' ? {'textureId': 1} : null,
        );
        tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          const MethodChannel('FlutterWebRTC/Texture1'),
          (call) async => null,
        );
        void expectFits() {
          final tiles = find.byType(ParticipantTile);
          expect(tiles, findsWidgets);
          for (final e in tiles.evaluate()) {
            expect(e.size!.shortestSide, greaterThanOrEqualTo(60));
          }
        }

        final session = _FakeSession(count);
        await tester.pumpWidget(MaterialApp(home: MeetingView(session: session)));
        await tester.pump();
        expectFits();
        await tester.tap(find.byType(ParticipantTile).first);
        await tester.pump();
        expectFits();
        expect(tester.takeException(), isNull);
        await tester.pumpWidget(const SizedBox());
        await tester.pump();
      });
    }
  }

  testWidgets('two people: the other fills the stage, we float and swap on tap', (tester) async {
    tester.view.physicalSize = const Size(390, 844);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final messenger = tester.binding.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(
      const MethodChannel('FlutterWebRTC.Method'),
      (call) async => call.method == 'createVideoRenderer' ? {'textureId': 1} : null,
    );
    messenger.setMockMethodCallHandler(const MethodChannel('FlutterWebRTC/Texture1'), (call) async => null);
    Finder tile(String id) => find.byWidgetPredicate((w) => w is ParticipantTile && w.participant.id == id);
    await tester.pumpWidget(MaterialApp(home: MeetingView(session: _FakeSession(2))));
    await tester.pump();
    expect(tester.getSize(tile('p1')).width, 374);
    expect(tester.getSize(tile('p0')).width, lessThan(130));
    final before = tester.getCenter(tile('p0'));
    await tester.drag(tile('p0'), const Offset(-300, -500));
    await tester.pumpAndSettle();
    expect(tester.getCenter(tile('p0')).dx, lessThan(before.dx));
    expect(tester.getCenter(tile('p0')).dy, lessThan(before.dy));
    await tester.tap(tile('p0'));
    await tester.pump();
    expect(tester.getSize(tile('p0')).width, 374);
    expect(tester.getSize(tile('p1')).width, lessThan(130));
    await tester.pumpWidget(const SizedBox());
    await tester.pump();
  });
}
