import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/material.dart';

import 'meeting_controller.dart';

/// A chat message. Messages stay on this device: the demo has no chat
/// backend, so other devices do not see them.
class ChatMessage {
  /// Creates a message.
  const ChatMessage(this.text, this.sentAt);

  /// Text.
  final String text;

  /// Local send time.
  final DateTime sentAt;
}

/// Group chat with a meeting entry point and an "active meeting" bar.
class ChatScreen extends StatefulWidget {
  /// Creates the screen.
  const ChatScreen({super.key, required this.meeting});

  /// App wide meeting controller.
  final MeetingController meeting;

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> {
  final _composer = TextEditingController();
  final _messages = <ChatMessage>[];

  @override
  void dispose() {
    _composer.dispose();
    super.dispose();
  }

  void _send() {
    final text = _composer.text.trim();
    if (text.isEmpty) return;
    setState(() => _messages.add(ChatMessage(text, DateTime.now())));
    _composer.clear();
  }

  @override
  Widget build(BuildContext context) {
    final session = widget.meeting.session;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Ecochain jamoasi'),
        actions: [
          ListenableBuilder(
            listenable: session,
            builder: (context, _) => IconButton(
              tooltip: session.isActive ? 'Uchrashuvga qaytish' : 'Uchrashuvni ochish',
              icon: Icon(session.isActive ? Icons.videocam : Icons.video_call_outlined),
              onPressed: () => widget.meeting.open(context),
            ),
          ),
        ],
      ),
      body: Column(
        children: [
          ListenableBuilder(
            listenable: session,
            builder: (context, _) => session.isActive
                ? _ActiveMeetingBar(session: session, onReturn: () => widget.meeting.open(context))
                : const SizedBox.shrink(),
          ),
          Expanded(
            child: ListView(
              padding: const EdgeInsets.all(16),
              children: [
                const Card(
                  child: ListTile(
                    leading: Icon(Icons.info_outline),
                    title: Text('Demo chati'),
                    subtitle: Text('Xabarlar faqat shu qurilmada saqlanadi va boshqa qurilmalarga yuborilmaydi.'),
                  ),
                ),
                for (final message in _messages)
                  Align(
                    alignment: Alignment.centerRight,
                    child: Card(
                      color: Theme.of(context).colorScheme.primaryContainer,
                      child: Padding(
                        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                        child: Text(message.text),
                      ),
                    ),
                  ),
              ],
            ),
          ),
          SafeArea(
            top: false,
            minimum: const EdgeInsets.fromLTRB(16, 0, 8, 12),
            child: Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _composer,
                    onSubmitted: (_) => _send(),
                    decoration: const InputDecoration(hintText: 'Xabar yozing'),
                  ),
                ),
                IconButton(onPressed: _send, tooltip: 'Yuborish', icon: const Icon(Icons.send)),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _ActiveMeetingBar extends StatelessWidget {
  const _ActiveMeetingBar({required this.session, required this.onReturn});

  final RtcSession session;
  final VoidCallback onReturn;

  @override
  Widget build(BuildContext context) {
    final reconnecting = session.connectionState == RtcConnectionState.reconnecting;
    return Material(
      color: reconnecting ? Colors.orange.shade100 : const Color(0xffd9f2ee),
      child: InkWell(
        onTap: onReturn,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
          child: Row(
            children: [
              Icon(reconnecting ? Icons.wifi_off : Icons.call, size: 20),
              const SizedBox(width: 12),
              Expanded(
                child: Text(
                  reconnecting
                      ? 'Uchrashuv: aloqa tiklanmoqda…'
                      : 'Faol uchrashuv · ${session.participants.length} ishtirokchi',
                ),
              ),
              FilledButton.tonal(onPressed: onReturn, child: const Text('Uchrashuvga qaytish')),
            ],
          ),
        ),
      ),
    );
  }
}
