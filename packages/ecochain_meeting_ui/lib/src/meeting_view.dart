import 'dart:async';

import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'keep_awake.dart';
import 'strings.dart';
import 'tile_layout.dart';

/// The in-call screen: gallery, status banner and controls.
///
/// The view never owns the session. Popping the route that shows it (for
/// example to go back to a chat) leaves the call running; only the leave
/// button or the host ending the meeting stops it. Opening a new
/// [MeetingView] for the same session shows the call as it is.
///
/// The screen does not auto-lock while the view shows an active call.
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

  final _keepAwake = KeepAwake();

  RtcSession get _session => widget.session;

  void _updateKeepAwake() => _keepAwake.set(_session.isActive);

  @override
  void initState() {
    super.initState();
    _session.addListener(_updateKeepAwake);
    _updateKeepAwake();
    _events = _session.events.listen((event) {
      if (event is RtcParticipantJoined && mounted) {
        _showNotice('${event.participant.name} qo‘shildi');
      }
    });
  }

  @override
  void dispose() {
    _session.removeListener(_updateKeepAwake);
    _keepAwake.set(false);
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
        // Two people: the other one fills the stage and we float on top of it.
        final remote = participants.where((p) => !p.isLocal).firstOrNull;
        final duo = presenter == null && participants.length == 2 && participants.any((p) => p.isLocal);
        final spotlight = presenter ?? pinned ?? (duo ? remote : null);
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
                          duo: duo,
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
      _ when session.sendingPaused => (Colors.orange.shade800, MeetingStrings.paused(session.sendPauseReasons.last)),
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
    required this.duo,
    required this.gallery,
    required this.onTap,
    required this.pinnedId,
  });

  final RtcParticipant? spotlight;
  final bool showScreen;

  /// Float the gallery's only participant over the spotlight.
  final bool duo;
  final List<RtcParticipant> gallery;
  final ValueChanged<RtcParticipant> onTap;
  final String? pinnedId;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final wide = constraints.maxWidth >= 720;
        final spot = spotlight;
        if (spot == null) return _Gallery(participants: gallery, onTap: onTap, pinnedId: pinnedId);
        final tile = ParticipantTile(
          participant: spot,
          showScreen: showScreen,
          onTap: () => onTap(spot),
          pinned: spot.id == pinnedId,
        );
        final main = Padding(padding: const EdgeInsets.all(8), child: tile);
        if (gallery.isEmpty) return main;
        if (duo) {
          final other = gallery.single;
          return Stack(
            fit: StackFit.expand,
            children: [
              main,
              _Floating(
                child: ParticipantTile(participant: other, onTap: () => onTap(other), compact: true),
              ),
            ],
          );
        }
        final strip = _Strip(
          participants: gallery,
          onTap: onTap,
          pinnedId: pinnedId,
          axis: wide ? Axis.vertical : Axis.horizontal,
        );
        // Desktop: spotlight left, strip right. Mobile: spotlight on top.
        return wide
            ? Row(
                children: [
                  Expanded(flex: 3, child: main),
                  SizedBox(width: 220, child: strip),
                ],
              )
            : Column(
                children: [
                  Expanded(flex: 3, child: main),
                  SizedBox(height: 160, child: strip),
                ],
              );
      },
    );
  }
}

const _gap = 8.0;

class _Gallery extends StatelessWidget {
  const _Gallery({required this.participants, required this.onTap, required this.pinnedId});

  final List<RtcParticipant> participants;
  final ValueChanged<RtcParticipant> onTap;
  final String? pinnedId;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(_gap),
      child: LayoutBuilder(
        builder: (context, constraints) {
          final area = constraints.biggest;
          final count = participants.length;
          final grid = tileGrid(count: count, area: area, gap: _gap);
          if (grid.scrolls) {
            return Center(
              child: SizedBox(
                width: grid.columns * (grid.tile.width + _gap) - _gap,
                child: GridView.builder(
                  padding: EdgeInsets.zero,
                  itemCount: count,
                  gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
                    crossAxisCount: grid.columns,
                    mainAxisSpacing: _gap,
                    crossAxisSpacing: _gap,
                    childAspectRatio: grid.tile.aspectRatio,
                  ),
                  itemBuilder: (context, index) => _tile(participants[index], onTap, pinnedId),
                ),
              ),
            );
          }
          final rects = tileRects(count: count, area: area, columns: grid.columns, tile: grid.tile, gap: _gap);
          return Stack(
            children: [
              for (final (i, p) in participants.indexed)
                Positioned.fromRect(key: ValueKey(p.id), rect: rects[i], child: _tile(p, onTap, pinnedId)),
            ],
          );
        },
      ),
    );
  }
}

/// A single line of small tiles next to the spotlight.
class _Strip extends StatelessWidget {
  const _Strip({required this.participants, required this.onTap, required this.pinnedId, required this.axis});

  final List<RtcParticipant> participants;
  final ValueChanged<RtcParticipant> onTap;
  final String? pinnedId;
  final Axis axis;

  @override
  Widget build(BuildContext context) {
    return ListView.builder(
      scrollDirection: axis,
      padding: const EdgeInsets.all(_gap / 2),
      itemCount: participants.length,
      itemBuilder: (context, index) => Padding(
        key: ValueKey(participants[index].id),
        padding: const EdgeInsets.all(_gap / 2),
        child: AspectRatio(
          aspectRatio: axis == Axis.vertical ? 16 / 9 : 3 / 4,
          child: _tile(participants[index], onTap, pinnedId, compact: true),
        ),
      ),
    );
  }
}

Widget _tile(RtcParticipant p, ValueChanged<RtcParticipant> onTap, String? pinnedId, {bool compact = false}) =>
    ParticipantTile(
      key: ValueKey(p.id),
      participant: p,
      onTap: () => onTap(p),
      pinned: p.id == pinnedId,
      compact: compact,
    );

/// The small picture of a two-person call. It can be dragged and settles in
/// the nearest corner.
class _Floating extends StatefulWidget {
  const _Floating({required this.child});

  final Widget child;

  @override
  State<_Floating> createState() => _FloatingState();
}

class _FloatingState extends State<_Floating> {
  static const _margin = 20.0;

  Alignment _corner = Alignment.bottomRight;
  Offset? _drag;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final size = floatingTileSize(constraints.biggest);
        final room = Rect.fromLTRB(
          _margin,
          _margin,
          constraints.maxWidth - size.width - _margin,
          constraints.maxHeight - size.height - _margin,
        );
        if (room.width < 0 || room.height < 0) return const SizedBox.shrink();
        final at = _drag ?? _corner.withinRect(room);
        return Stack(
          children: [
            AnimatedPositioned(
              duration: _drag == null ? const Duration(milliseconds: 200) : Duration.zero,
              curve: Curves.easeOut,
              left: at.dx,
              top: at.dy,
              width: size.width,
              height: size.height,
              child: GestureDetector(
                onPanStart: (_) => setState(() => _drag = at),
                onPanUpdate: (d) => setState(() {
                  final next = (_drag ?? at) + d.delta;
                  _drag = Offset(next.dx.clamp(room.left, room.right), next.dy.clamp(room.top, room.bottom));
                }),
                onPanEnd: (_) => setState(() {
                  final p = _drag ?? at;
                  _corner = Alignment(p.dx < room.center.dx ? -1 : 1, p.dy < room.center.dy ? -1 : 1);
                  _drag = null;
                }),
                onPanCancel: () => setState(() => _drag = null),
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    borderRadius: BorderRadius.circular(12),
                    boxShadow: const [BoxShadow(color: Colors.black54, blurRadius: 12)],
                  ),
                  child: widget.child,
                ),
              ),
            ),
          ],
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
    this.compact = false,
  });

  /// Who to show.
  final RtcParticipant participant;

  /// Show the screen share instead of the camera.
  final bool showScreen;

  /// Tap handler (pin/unpin).
  final VoidCallback? onTap;

  /// Draw the pinned marker.
  final bool pinned;

  /// Smaller avatar, for small tiles.
  final bool compact;

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
              placeholder: Center(
                child: CircleAvatar(
                  radius: compact ? 22 : 32,
                  backgroundColor: const Color(0xff00776d),
                  child: Text(
                    initials,
                    style: TextStyle(fontSize: compact ? 18 : 26, color: Colors.white),
                  ),
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
    final canSend = active && !session.sendingPaused;
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
            onPressed: canSend ? () => onError(() => session.setMicrophoneEnabled(!session.microphoneEnabled)) : null,
          ),
          _RoundButton(
            icon: session.cameraEnabled ? Icons.videocam : Icons.videocam_off,
            label: MeetingStrings.camera,
            off: !session.cameraEnabled,
            onPressed: canSend ? () => onError(() => session.setCameraEnabled(!session.cameraEnabled)) : null,
          ),
          if (mobile)
            _RoundButton(
              icon: Icons.cameraswitch_outlined,
              label: MeetingStrings.switchCamera,
              onPressed: canSend && session.cameraEnabled ? () => onError(session.switchCamera) : null,
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
            disabledBackgroundColor: const Color(0xff22282e),
            disabledForegroundColor: Colors.white38,
            minimumSize: const Size(52, 52),
          ),
          onPressed: onPressed,
          icon: Icon(icon),
        ),
      ),
    );
  }
}
