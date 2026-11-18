import 'dart:async';

import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'strings.dart';

/// The in-call screen: gallery, status banner and controls.
///
/// The view never owns the session. Popping the route that shows it (for
/// example to go back to a chat) leaves the call running; only the leave
/// button or the host ending the meeting stops it. Opening a new
/// [MeetingView] for the same session shows the call as it is.
///
/// [onMinimize] adds a back/minimize button, used by apps that embed the
/// meeting next to other screens.
///
/// ```dart
/// Navigator.of(context).push(MaterialPageRoute(
///   builder: (_) => MeetingView(session: controller.session, onMinimize: Navigator.of(context).pop),
/// ));
/// ```
class MeetingView extends StatefulWidget {
  /// Creates the view.
  const MeetingView({super.key, required this.session, this.onMinimize, this.title = 'Ecochain Meet'});

  /// Session to show.
  final RtcSession session;

  /// Called by the minimize button; hidden when `null`.
  final VoidCallback? onMinimize;

  /// App bar title.
  final String title;

  @override
  State<MeetingView> createState() => _MeetingViewState();
}

class _MeetingViewState extends State<MeetingView> {
  String? _pinned;
  String? _notice;
  StreamSubscription<RtcEvent>? _events;

  RtcSession get _session => widget.session;

  @override
  void initState() {
    super.initState();
    _events = _session.events.listen((event) {
      if (event is RtcParticipantJoined && mounted) {
        _showNotice('${event.participant.name} qo‘shildi');
      }
    });
  }

  @override
  void dispose() {
    unawaited(_events?.cancel());
    super.dispose();
  }

  void _showNotice(String text) {
    setState(() => _notice = text);
    Future<void>.delayed(const Duration(seconds: 3), () {
      if (mounted && _notice == text) setState(() => _notice = null);
    });
  }

  Future<void> _run(Future<void> Function() action) async {
    try {
      await action();
    } on RtcException catch (e) {
      if (mounted) _showNotice(MeetingStrings.error(e.code));
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: _session,
      builder: (context, _) {
        final session = _session;
        final participants = session.participants;
        final pinned = participants.where((p) => p.id == _pinned).firstOrNull;
        final presenter = participants.where((p) => p.screen != null && !p.screen!.muted).firstOrNull;
        final spotlight = presenter ?? pinned;
        final gallery = [
          for (final p in participants)
            if (p.id != spotlight?.id || presenter != null) p,
        ];
        _reportLayout(gallery, spotlight);
        return Scaffold(
          backgroundColor: const Color(0xff101418),
          appBar: AppBar(
            backgroundColor: const Color(0xff101418),
            foregroundColor: Colors.white,
            leading: widget.onMinimize == null
                ? null
                : IconButton(
                    tooltip: 'Chatga qaytish',
                    icon: const Icon(Icons.arrow_back),
                    onPressed: widget.onMinimize,
                  ),
            title: Text(widget.title),
            actions: [
              Center(
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 12),
                  child: Text('${participants.length} ${MeetingStrings.participants.toLowerCase()}'),
                ),
              ),
            ],
          ),
          body: SafeArea(
            child: Column(
              children: [
                _StatusBanner(session: session, notice: _notice),
                Expanded(
                  child: session.connectionState == RtcConnectionState.disconnected
                      ? Center(
                          child: Text(
                            MeetingStrings.reason(session.disconnectReason),
                            style: const TextStyle(color: Colors.white70, fontSize: 16),
                            textAlign: TextAlign.center,
                          ),
                        )
                      : _Stage(
                          spotlight: spotlight,
                          showScreen: presenter != null,
                          gallery: gallery,
                          onTap: (p) => setState(() => _pinned = _pinned == p.id ? null : p.id),
                          pinnedId: _pinned,
                        ),
                ),
                _Controls(session: session, onError: _run),
              ],
            ),
          ),
        );
      },
    );
  }

  List<String>? _lastVisible;
  String? _lastPinned;

  void _reportLayout(List<RtcParticipant> gallery, RtcParticipant? spotlight) {
    final visible = [
      for (final p in gallery)
        if (!p.isLocal) p.id,
    ];
    if (listEquals(visible, _lastVisible) && _lastPinned == spotlight?.id) return;
    _lastVisible = visible;
    _lastPinned = spotlight?.id;
    _session.setVideoLayout(visibleParticipantIds: visible, pinnedParticipantId: spotlight?.id);
  }
}

class _StatusBanner extends StatelessWidget {
  const _StatusBanner({required this.session, required this.notice});

  final RtcSession session;
  final String? notice;

  @override
  Widget build(BuildContext context) {
    final state = session.connectionState;
    final (color, text) = switch (state) {
      RtcConnectionState.connecting => (Colors.blueGrey, MeetingStrings.state(state)),
      RtcConnectionState.reconnecting => (Colors.orange.shade800, MeetingStrings.state(state)),
      RtcConnectionState.disconnected => (Colors.red.shade800, MeetingStrings.state(state)),
      _ => switch (session.lastError) {
        // A device that failed to open is shown until the next successful
        // toggle, so a silently missing camera is never a mystery.
        final error? when notice == null => (Colors.red.shade900, MeetingStrings.error(error.code)),
        _ => (null, notice),
      },
    };
    if (text == null) return const SizedBox(height: 4);
    return Container(
      width: double.infinity,
      color: color ?? const Color(0xff2a3138),
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      child: Row(
        children: [
          if (state == RtcConnectionState.reconnecting || state == RtcConnectionState.connecting) ...[
            const SizedBox.square(dimension: 14, child: CircularProgressIndicator(strokeWidth: 2, color: Colors.white)),
            const SizedBox(width: 10),
          ],
          Expanded(
            child: Text(text, style: const TextStyle(color: Colors.white)),
          ),
        ],
      ),
    );
  }
}

class _Stage extends StatelessWidget {
  const _Stage({
    required this.spotlight,
    required this.showScreen,
    required this.gallery,
    required this.onTap,
    required this.pinnedId,
  });

  final RtcParticipant? spotlight;
  final bool showScreen;
  final List<RtcParticipant> gallery;
  final ValueChanged<RtcParticipant> onTap;
  final String? pinnedId;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final wide = constraints.maxWidth >= 720;
        final grid = _Gallery(participants: gallery, onTap: onTap, pinnedId: pinnedId, wide: wide);
        final spot = spotlight;
        if (spot == null) return grid;
        final main = Padding(
          padding: const EdgeInsets.all(8),
          child: ParticipantTile(participant: spot, showScreen: showScreen, onTap: () => onTap(spot), pinned: true),
        );
        if (gallery.isEmpty) return main;
        // Desktop: spotlight left, strip right. Mobile: spotlight on top.
        return wide
            ? Row(
                children: [
                  Expanded(flex: 3, child: main),
                  SizedBox(width: 280, child: grid),
                ],
              )
            : Column(
                children: [
                  Expanded(flex: 3, child: main),
                  SizedBox(height: 150, child: grid),
                ],
              );
      },
    );
  }
}

class _Gallery extends StatelessWidget {
  const _Gallery({required this.participants, required this.onTap, required this.pinnedId, required this.wide});

  final List<RtcParticipant> participants;
  final ValueChanged<RtcParticipant> onTap;
  final String? pinnedId;
  final bool wide;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final count = participants.length;
        // Mobile: one column for 1-2 people, two columns beyond. Desktop:
        // roughly square grid.
        final columns = wide ? (count <= 1 ? 1 : (count <= 4 ? 2 : (count <= 9 ? 3 : 4))) : (count <= 2 ? 1 : 2);
        final rows = (count / columns).ceil().clamp(1, 1000);
        final fitHeight = constraints.maxHeight / rows - 8;
        final tileWidth = constraints.maxWidth / columns - 8;
        final aspect = (tileWidth / fitHeight.clamp(120, 10000)).clamp(0.6, 16 / 9);
        return GridView.builder(
          padding: const EdgeInsets.all(4),
          itemCount: count,
          gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
            crossAxisCount: columns,
            childAspectRatio: aspect,
            mainAxisSpacing: 8,
            crossAxisSpacing: 8,
          ),
          itemBuilder: (context, index) {
            final p = participants[index];
            return ParticipantTile(
              key: ValueKey(p.id),
              participant: p,
              onTap: () => onTap(p),
              pinned: p.id == pinnedId,
            );
          },
        );
      },
    );
  }
}

/// One participant: video or initials, name, microphone and connection
/// indicators. Highlighted while speaking.
class ParticipantTile extends StatelessWidget {
  /// Creates a tile.
  const ParticipantTile({
    super.key,
    required this.participant,
    this.showScreen = false,
    this.onTap,
    this.pinned = false,
  });

  /// Who to show.
  final RtcParticipant participant;

  /// Show the screen share instead of the camera.
  final bool showScreen;

  /// Tap handler (pin/unpin).
  final VoidCallback? onTap;

  /// Draw the pinned marker.
  final bool pinned;

  @override
  Widget build(BuildContext context) {
    final p = participant;
    final track = showScreen ? p.screen : p.camera;
    final initials = p.name.isEmpty ? '?' : p.name.characters.first.toUpperCase();
    return GestureDetector(
      onTap: onTap,
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 200),
        decoration: BoxDecoration(
          color: const Color(0xff1f262d),
          borderRadius: BorderRadius.circular(12),
          border: Border.all(color: p.isSpeaking ? const Color(0xff2ecc71) : Colors.transparent, width: 3),
        ),
        clipBehavior: Clip.antiAlias,
        child: Stack(
          fit: StackFit.expand,
          children: [
            RtcVideoView(
              track: track,
              mirror: p.isLocal && !showScreen,
              cover: !showScreen,
              placeholder: Center(
                child: CircleAvatar(
                  radius: 32,
                  backgroundColor: const Color(0xff00776d),
                  child: Text(initials, style: const TextStyle(fontSize: 26, color: Colors.white)),
                ),
              ),
            ),
            Positioned(
              left: 8,
              bottom: 8,
              right: 8,
              child: Row(
                children: [
                  Flexible(
                    child: Container(
                      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
                      decoration: BoxDecoration(color: Colors.black54, borderRadius: BorderRadius.circular(6)),
                      child: Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Icon(p.microphoneOn ? Icons.mic : Icons.mic_off, size: 14, color: Colors.white),
                          const SizedBox(width: 4),
                          Flexible(
                            child: Text(
                              p.isLocal ? '${p.name} ${MeetingStrings.you}' : p.name,
                              overflow: TextOverflow.ellipsis,
                              style: const TextStyle(color: Colors.white, fontSize: 13),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                ],
              ),
            ),
            if (!p.connected)
              const Positioned(top: 8, right: 8, child: Icon(Icons.wifi_off, color: Colors.orangeAccent, size: 18)),
            if (pinned && !showScreen)
              const Positioned(top: 8, left: 8, child: Icon(Icons.push_pin, color: Colors.white70, size: 16)),
          ],
        ),
      ),
    );
  }
}

class _Controls extends StatelessWidget {
  const _Controls({required this.session, required this.onError});

  final RtcSession session;
  final Future<void> Function(Future<void> Function()) onError;

  @override
  Widget build(BuildContext context) {
    final active = session.connectionState == RtcConnectionState.connected;
    final mobile = defaultTargetPlatform == TargetPlatform.android || defaultTargetPlatform == TargetPlatform.iOS;
    final desktop = defaultTargetPlatform == TargetPlatform.macOS || defaultTargetPlatform == TargetPlatform.windows;
    return Container(
      color: const Color(0xff161b20),
      padding: const EdgeInsets.symmetric(vertical: 12, horizontal: 8),
      child: Wrap(
        alignment: WrapAlignment.center,
        spacing: 12,
        runSpacing: 8,
        children: [
          _RoundButton(
            icon: session.microphoneEnabled ? Icons.mic : Icons.mic_off,
            label: MeetingStrings.microphone,
            off: !session.microphoneEnabled,
            onPressed: active ? () => onError(() => session.setMicrophoneEnabled(!session.microphoneEnabled)) : null,
          ),
          _RoundButton(
            icon: session.cameraEnabled ? Icons.videocam : Icons.videocam_off,
            label: MeetingStrings.camera,
            off: !session.cameraEnabled,
            onPressed: active ? () => onError(() => session.setCameraEnabled(!session.cameraEnabled)) : null,
          ),
          if (mobile)
            _RoundButton(
              icon: Icons.cameraswitch_outlined,
              label: MeetingStrings.switchCamera,
              onPressed: active && session.cameraEnabled ? () => onError(session.switchCamera) : null,
            ),
          if (desktop)
            _RoundButton(
              icon: session.screenShareEnabled ? Icons.stop_screen_share_outlined : Icons.screen_share_outlined,
              label: MeetingStrings.screen,
              onPressed: active
                  ? () => onError(session.screenShareEnabled ? session.stopScreenShare : session.startScreenShare)
                  : null,
            ),
          _RoundButton(
            icon: Icons.call_end,
            label: MeetingStrings.leave,
            danger: true,
            onPressed: session.isActive ? () => onError(session.leave) : null,
          ),
          if (session.isHost)
            TextButton(
              onPressed: active ? () => onError(session.endMeeting) : null,
              child: const Text(MeetingStrings.endForAll, style: TextStyle(color: Colors.redAccent)),
            ),
        ],
      ),
    );
  }
}

class _RoundButton extends StatelessWidget {
  const _RoundButton({
    required this.icon,
    required this.label,
    required this.onPressed,
    this.off = false,
    this.danger = false,
  });

  final IconData icon;
  final String label;
  final VoidCallback? onPressed;
  final bool off;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final background = danger ? Colors.red.shade700 : (off ? Colors.white : const Color(0xff2a3138));
    final foreground = danger ? Colors.white : (off ? Colors.black87 : Colors.white);
    return Tooltip(
      message: label,
      child: Semantics(
        button: true,
        label: label,
        child: IconButton.filled(
          style: IconButton.styleFrom(
            backgroundColor: background,
            foregroundColor: foreground,
            minimumSize: const Size(52, 52),
          ),
          onPressed: onPressed,
          icon: Icon(icon),
        ),
      ),
    );
  }
}
