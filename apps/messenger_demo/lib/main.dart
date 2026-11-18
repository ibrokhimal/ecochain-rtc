import 'package:flutter/material.dart';
import 'package:ecochain_meeting_ui/ecochain_meeting_ui.dart';

import 'chat_screen.dart';
import 'meeting_controller.dart';

void main() => runApp(const MessengerApp());

/// Messenger demo: a group chat with an embedded meeting.
class MessengerApp extends StatefulWidget {
  /// Creates the app.
  const MessengerApp({super.key});

  @override
  State<MessengerApp> createState() => _MessengerAppState();
}

class _MessengerAppState extends State<MessengerApp> {
  final _meeting = MeetingController();

  @override
  void dispose() {
    _meeting.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'Ecochain Messenger',
    theme: meetingTheme(),
    home: ChatScreen(meeting: _meeting),
  );
}
