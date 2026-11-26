import 'package:ecochain_meeting_ui/ecochain_meeting_ui.dart';
import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wakelock_plus/wakelock_plus.dart';
import 'package:wakelock_plus_platform_interface/wakelock_plus_platform_interface.dart';

class _FakeWakelock extends WakelockPlusPlatformInterface {
  final log = <bool>[];

  @override
  bool get isMock => true;

  @override
  Future<void> toggle({required bool enable}) async => log.add(enable);

  @override
  Future<bool> get enabled async => log.isNotEmpty && log.last;
}

class _FakeSession extends RtcSession {
  bool active = true;

  @override
  bool get isActive => active;

  @override
  void setVideoLayout({required List<String> visibleParticipantIds, String? pinnedParticipantId}) {}

  void end() {
    active = false;
    notifyListeners();
  }
}

void main() {
  late _FakeWakelock wakelock;
  setUp(() => wakelockPlusPlatformInstance = wakelock = _FakeWakelock());

  testWidgets('the screen stays on while the meeting is shown', (tester) async {
    await tester.pumpWidget(MaterialApp(home: MeetingView(session: _FakeSession())));
    expect(wakelock.log, [true]);
    // Replaced by another meeting screen: never released in between.
    await tester.pumpWidget(MaterialApp(home: MeetingView(key: UniqueKey(), session: _FakeSession())));
    expect(wakelock.log, [true]);
    await tester.pumpWidget(const SizedBox());
    expect(wakelock.log, [true, false]);
  });

  testWidgets('the screen may lock again once the meeting ended', (tester) async {
    final session = _FakeSession();
    await tester.pumpWidget(MaterialApp(home: MeetingView(session: session)));
    session.end();
    expect(wakelock.log, [true, false]);
    await tester.pumpWidget(const SizedBox());
    expect(wakelock.log, [true, false]);
  });
}
