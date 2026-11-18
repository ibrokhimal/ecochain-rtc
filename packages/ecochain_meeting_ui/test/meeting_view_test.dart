import 'package:ecochain_meeting_ui/ecochain_meeting_ui.dart';
import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('closing and reopening the meeting screen keeps the same session', (tester) async {
    final session = RtcSession();
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => Scaffold(
            body: Center(
              child: FilledButton(
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (route) => MeetingView(session: session, onMinimize: Navigator.of(route).pop),
                  ),
                ),
                child: const Text('open'),
              ),
            ),
          ),
        ),
      ),
    );

    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    expect(find.byType(MeetingView), findsOneWidget);

    await tester.tap(find.byTooltip('Chatga qaytish'));
    await tester.pumpAndSettle();
    expect(find.byType(MeetingView), findsNothing);
    // Leaving the screen must not touch the session.
    expect(session.connectionState, RtcConnectionState.idle);

    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    expect(find.byType(MeetingView), findsOneWidget);
    session.dispose();
  });

  testWidgets('ended sessions show the reason', (tester) async {
    final session = RtcSession();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: JoinPanel(onJoin: (_) async {}, lastEndReason: RtcDisconnectReason.roomEnded),
        ),
      ),
    );
    expect(find.text(MeetingStrings.reason(RtcDisconnectReason.roomEnded)), findsOneWidget);
    session.dispose();
  });
}
