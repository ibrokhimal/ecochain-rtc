import 'package:ecochain_meet_app/main.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('shows the join form when no meeting is active', (tester) async {
    await tester.pumpWidget(const MeetApp());
    expect(find.text('Qo‘shilish'), findsOneWidget);
    expect(find.text('Server manzili'), findsOneWidget);
  });
}
