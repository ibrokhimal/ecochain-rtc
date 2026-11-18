import 'package:ecochain_meeting_ui/ecochain_meeting_ui.dart';
import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/material.dart';

/// Default server, overridable with `--dart-define=ECOCHAIN_RTC_URL=...`.
const serverUrl = String.fromEnvironment('ECOCHAIN_RTC_URL', defaultValue: 'ws://127.0.0.1:7880/v1/rtc');

/// Development switches, set with `--dart-define`:
/// `ECOCHAIN_AUTOJOIN=true`, `ECOCHAIN_ROOM`, `ECOCHAIN_IDENTITY`,
/// `ECOCHAIN_NAME`, `ECOCHAIN_MIC`, `ECOCHAIN_CAMERA`.
const autoJoin = bool.fromEnvironment('ECOCHAIN_AUTOJOIN');
const _room = String.fromEnvironment('ECOCHAIN_ROOM', defaultValue: 'demo');
const _mic = bool.fromEnvironment('ECOCHAIN_MIC', defaultValue: true);
const _camera = bool.fromEnvironment('ECOCHAIN_CAMERA', defaultValue: true);
const _identity = String.fromEnvironment('ECOCHAIN_IDENTITY', defaultValue: 'messenger-user');
const _name = String.fromEnvironment('ECOCHAIN_NAME', defaultValue: 'Messenger foydalanuvchisi');

/// Owns the meeting session for the whole app.
///
/// The session lives here, above the navigator, so the chat screen and the
/// meeting screen can come and go while the call keeps running.
class MeetingController {
  /// The single meeting session of this app.
  final session = RtcSession();

  /// Opens the meeting screen (join form first if no call is active).
  /// Popping the route returns to the chat without ending the call.
  Future<void> open(BuildContext context) =>
      Navigator.of(context).push<void>(MaterialPageRoute(builder: (_) => _MeetingRoute(session: session)));

  /// Releases the session.
  void dispose() => session.dispose();
}

class _MeetingRoute extends StatelessWidget {
  const _MeetingRoute({required this.session});

  final RtcSession session;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: session,
      builder: (context, _) {
        if (session.isActive) {
          return MeetingView(
            session: session,
            title: 'Jamoa uchrashuvi',
            onMinimize: () => Navigator.of(context).pop(),
          );
        }
        return Scaffold(
          appBar: AppBar(title: const Text('Jamoa uchrashuvi')),
          body: JoinPanel(
            title: 'Uchrashuvga qo‘shilish',
            initialUrl: serverUrl,
            room: _room,
            identity: _identity,
            displayName: _name,
            lastEndReason: session.disconnectReason,
            initialMicrophone: _mic,
            initialCamera: _camera,
            autoJoin: autoJoin,
            onJoin: (request) => session.connect(
              RtcConnectOptions(
                url: request.url,
                token: request.token,
                microphone: request.microphone,
                camera: request.camera,
                allowInsecureLocalhost: true,
              ),
            ),
          ),
        );
      },
    );
  }
}
