import 'package:ecochain_messenger_demo/main.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('chat keeps messages while the meeting screen opens and closes', (tester) async {
    await tester.pumpWidget(const MessengerApp());
    await tester.enterText(find.byType(TextField), 'Salom jamoa');
    await tester.tap(find.byTooltip('Yuborish'));
    await tester.pump();
    expect(find.text('Salom jamoa'), findsOneWidget);
    expect(find.text('Uchrashuvga qaytish'), findsNothing, reason: 'no active meeting yet');

    await tester.tap(find.byTooltip('Uchrashuvni ochish'));
    await tester.pumpAndSettle();
    expect(find.text('Uchrashuvga qo‘shilish'), findsOneWidget);

    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(find.text('Salom jamoa'), findsOneWidget);
  });
}
