import 'package:ecochain_meeting_ui/ecochain_meeting_ui.dart';
import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/material.dart';

/// Default server, overridable with `--dart-define=ECOCHAIN_RTC_URL=...`.
const _serverUrl = String.fromEnvironment('ECOCHAIN_RTC_URL', defaultValue: 'ws://127.0.0.1:7880/v1/rtc');

/// Development switches, set with `--dart-define`:
/// `ECOCHAIN_AUTOJOIN=true`, `ECOCHAIN_ROOM`, `ECOCHAIN_IDENTITY`,
/// `ECOCHAIN_NAME`, `ECOCHAIN_MIC`, `ECOCHAIN_CAMERA`.
const _autoJoin = bool.fromEnvironment('ECOCHAIN_AUTOJOIN');
const _room = String.fromEnvironment('ECOCHAIN_ROOM', defaultValue: 'demo');
const _mic = bool.fromEnvironment('ECOCHAIN_MIC', defaultValue: true);
const _camera = bool.fromEnvironment('ECOCHAIN_CAMERA', defaultValue: true);
const _identity = String.fromEnvironment('ECOCHAIN_IDENTITY', defaultValue: 'meet-user');
const _name = String.fromEnvironment('ECOCHAIN_NAME', defaultValue: 'Meet foydalanuvchisi');

void main() => runApp(const MeetApp());

/// Standalone Meet: a join form, then the shared meeting screen.
class MeetApp extends StatefulWidget {
  /// Creates the app.
  const MeetApp({super.key});

  @override
  State<MeetApp> createState() => _MeetAppState();
}

class _MeetAppState extends State<MeetApp> {
  /// Owned by the app, not by a screen, so rebuilding or replacing screens
  /// never ends the call.
  final _session = RtcSession();

  @override
  void dispose() {
    _session.dispose();
    super.dispose();
  }

  Future<void> _join(JoinRequest request) => _session.connect(
    RtcConnectOptions(
      url: request.url,
      token: request.token,
      microphone: request.microphone,
      camera: request.camera,
      systemCall: MeetingStrings.systemCall('Ecochain Meet'),
      allowInsecureLocalhost: true,
    ),
  );

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'Ecochain Meet',
    theme: meetingTheme(),
    home: ListenableBuilder(
      listenable: _session,
      builder: (context, _) => _session.isActive
          ? MeetingView(session: _session)
          : Scaffold(
              body: SafeArea(
                child: JoinPanel(
                  onJoin: _join,
                  initialUrl: _serverUrl,
                  room: _room,
                  identity: _identity,
                  displayName: _name,
                  lastEndReason: _session.disconnectReason,
                  initialMicrophone: _mic,
                  initialCamera: _camera,
                  autoJoin: _autoJoin,
                ),
              ),
            ),
    ),
  );
}
