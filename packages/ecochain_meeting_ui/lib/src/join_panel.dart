import 'dart:async';

import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/material.dart';

import 'dev_token.dart';
import 'strings.dart';

/// Values the user entered in [JoinPanel].
class JoinRequest {
  /// Creates a request.
  const JoinRequest({required this.url, required this.token, required this.microphone, required this.camera});

  /// Signaling URL.
  final Uri url;

  /// Room token.
  final String token;

  /// Start with the microphone on.
  final bool microphone;

  /// Start with the camera on.
  final bool camera;
}

/// Form for joining a meeting with a server URL and token.
///
/// In development the "get development token" button fills the token from
/// the server's `/v1/dev/token` endpoint using [room], [identity] and
/// [displayName]. Real apps hide the form and pass a backend token straight
/// to [RtcSession.connect].
///
/// [onJoin] is called with the entered values; it should call
/// [RtcSession.connect] and may throw [RtcException], which is shown inline.
class JoinPanel extends StatefulWidget {
  /// Creates the panel.
  const JoinPanel({
    super.key,
    required this.onJoin,
    this.initialUrl = 'ws://127.0.0.1:7880/v1/rtc',
    this.room = 'demo',
    this.identity = 'user',
    this.displayName,
    this.title = 'Ecochain Meet',
    this.lastEndReason,
    this.initialMicrophone = true,
    this.initialCamera = true,
    this.autoJoin = false,
  });

  /// Called when the user taps join.
  final Future<void> Function(JoinRequest request) onJoin;

  /// Prefilled server URL.
  final String initialUrl;

  /// Room for development tokens.
  final String room;

  /// Identity for development tokens.
  final String identity;

  /// Display name for development tokens.
  final String? displayName;

  /// Heading.
  final String title;

  /// Reason of the previous session's end, shown as a notice.
  final RtcDisconnectReason? lastEndReason;

  /// Initial microphone switch position.
  final bool initialMicrophone;

  /// Initial camera switch position.
  final bool initialCamera;

  /// Development aid: fetch a development token and join right after the
  /// panel appears (used for unattended two-device tests).
  final bool autoJoin;

  @override
  State<JoinPanel> createState() => _JoinPanelState();
}

class _JoinPanelState extends State<JoinPanel> {
  late final _url = TextEditingController(text: widget.initialUrl);
  late final _room = TextEditingController(text: widget.room);
  late final _identity = TextEditingController(text: widget.identity);
  late final _name = TextEditingController(text: widget.displayName ?? widget.identity);
  final _token = TextEditingController();
  late bool _microphone = widget.initialMicrophone;
  late bool _camera = widget.initialCamera;
  bool _host = false;
  bool _busy = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    if (widget.autoJoin && widget.lastEndReason == null) {
      WidgetsBinding.instance.addPostFrameCallback((_) => unawaited(_join()));
    }
  }

  @override
  void dispose() {
    for (final c in [_url, _room, _identity, _name, _token]) {
      c.dispose();
    }
    super.dispose();
  }

  Future<void> _fetchToken() async {
    setState(() => _error = null);
    try {
      final token = await fetchDevToken(
        Uri.parse(_url.text.trim()),
        room: _room.text.trim(),
        identity: _identity.text.trim(),
        name: _name.text.trim(),
        host: _host,
      );
      if (mounted) setState(() => _token.text = token);
    } catch (e) {
      if (mounted) setState(() => _error = 'Token olinmadi: $e');
    }
  }

  Future<void> _join() async {
    if (_busy) return; // a second tap must not start a second call
    if (_token.text.trim().isEmpty) await _fetchToken();
    if (!mounted || _token.text.trim().isEmpty) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      if (_microphone || _camera) {
        await RtcSession.requestPermissions(microphone: _microphone, camera: _camera);
      }
      await widget.onJoin(
        JoinRequest(
          url: Uri.parse(_url.text.trim()),
          token: _token.text.trim(),
          microphone: _microphone,
          camera: _camera,
        ),
      );
    } on RtcException catch (e) {
      if (mounted) setState(() => _error = MeetingStrings.error(e.code));
    } on FormatException {
      if (mounted) setState(() => _error = MeetingStrings.error(RtcErrorCode.invalidUrl));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: ListView(
          padding: const EdgeInsets.all(24),
          shrinkWrap: true,
          children: [
            Text(widget.title, style: theme.textTheme.headlineSmall),
            if (widget.lastEndReason != null) ...[
              const SizedBox(height: 12),
              Card(
                child: ListTile(
                  leading: const Icon(Icons.info_outline),
                  title: Text(MeetingStrings.reason(widget.lastEndReason)),
                ),
              ),
            ],
            const SizedBox(height: 16),
            TextField(
              controller: _url,
              decoration: const InputDecoration(labelText: MeetingStrings.serverUrl, hintText: 'wss://…/v1/rtc'),
              keyboardType: TextInputType.url,
              autocorrect: false,
            ),
            Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _room,
                    decoration: const InputDecoration(labelText: MeetingStrings.room),
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: TextField(
                    controller: _identity,
                    decoration: const InputDecoration(labelText: 'ID'),
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: TextField(
                    controller: _name,
                    decoration: const InputDecoration(labelText: MeetingStrings.name),
                  ),
                ),
              ],
            ),
            TextField(
              controller: _token,
              obscureText: true,
              autocorrect: false,
              enableSuggestions: false,
              decoration: const InputDecoration(labelText: MeetingStrings.token),
            ),
            CheckboxListTile(
              value: _host,
              onChanged: (v) => setState(() => _host = v ?? false),
              title: const Text(MeetingStrings.host),
              contentPadding: EdgeInsets.zero,
            ),
            Align(
              alignment: Alignment.centerLeft,
              child: TextButton.icon(
                onPressed: _busy ? null : () => unawaited(_fetchToken()),
                icon: const Icon(Icons.key_outlined),
                label: const Text(MeetingStrings.devToken),
              ),
            ),
            SwitchListTile(
              value: _microphone,
              onChanged: (v) => setState(() => _microphone = v),
              title: const Text(MeetingStrings.microphone),
              secondary: Icon(_microphone ? Icons.mic : Icons.mic_off),
              contentPadding: EdgeInsets.zero,
            ),
            SwitchListTile(
              value: _camera,
              onChanged: (v) => setState(() => _camera = v),
              title: const Text(MeetingStrings.camera),
              secondary: Icon(_camera ? Icons.videocam : Icons.videocam_off),
              contentPadding: EdgeInsets.zero,
            ),
            if (_error case final error?)
              Padding(
                padding: const EdgeInsets.only(bottom: 12),
                child: Text(error, style: TextStyle(color: theme.colorScheme.error)),
              ),
            FilledButton.icon(
              onPressed: _busy ? null : () => unawaited(_join()),
              icon: _busy
                  ? const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2))
                  : const Icon(Icons.login),
              label: const Text(MeetingStrings.join),
            ),
          ],
        ),
      ),
    );
  }
}
