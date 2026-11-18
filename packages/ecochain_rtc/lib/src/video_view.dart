import 'dart:async';

import 'package:flutter/widgets.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';

import 'participant.dart';

/// Shows a video [RtcTrack] (local or remote).
///
/// The widget owns its native renderer: it is created when the widget
/// mounts and released when it unmounts, so tiles can be scrolled in and out
/// freely. Media of the track itself belongs to the session and is not
/// affected by this widget's lifetime.
///
/// Shows [placeholder] while the track is muted or no frame has arrived.
///
/// ```dart
/// RtcVideoView(
///   track: participant.camera,
///   mirror: participant.isLocal,
///   placeholder: Center(child: Text(participant.name)),
/// )
/// ```
class RtcVideoView extends StatefulWidget {
  /// Creates a view.
  const RtcVideoView({
    super.key,
    required this.track,
    this.mirror = false,
    this.cover = true,
    this.placeholder = const SizedBox.shrink(),
  });

  /// Track to render. `null` or an audio track shows [placeholder].
  final RtcTrack? track;

  /// Mirror horizontally; use for the local front camera.
  final bool mirror;

  /// Fill the area (cropping) instead of letterboxing. Use `false` for
  /// screen shares so no content is cut off.
  final bool cover;

  /// Shown when there is nothing to render.
  final Widget placeholder;

  @override
  State<RtcVideoView> createState() => _RtcVideoViewState();
}

class _RtcVideoViewState extends State<RtcVideoView> {
  final _renderer = RTCVideoRenderer();
  bool _ready = false;
  bool _hasFrame = false;
  bool _disposed = false;

  @override
  void initState() {
    super.initState();
    unawaited(_init());
  }

  Future<void> _init() async {
    await _renderer.initialize();
    if (_disposed) {
      await _renderer.dispose();
      return;
    }
    _renderer.onFirstFrameRendered = () {
      if (mounted) setState(() => _hasFrame = true);
    };
    setState(() => _ready = true);
    _attach();
  }

  @override
  void didUpdateWidget(RtcVideoView oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.track?.renderStream != widget.track?.renderStream) {
      _hasFrame = false;
      _attach();
    }
  }

  void _attach() {
    if (!_ready) return;
    final track = widget.track;
    _renderer.srcObject = track != null && track.canRender ? track.renderStream : null;
  }

  @override
  void dispose() {
    _disposed = true;
    if (_ready) {
      _renderer.srcObject = null;
      unawaited(_renderer.dispose());
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final track = widget.track;
    final visible = _ready && track != null && track.canRender;
    return Stack(
      fit: StackFit.expand,
      children: [
        if (visible)
          RTCVideoView(
            _renderer,
            mirror: widget.mirror,
            objectFit: widget.cover
                ? RTCVideoViewObjectFit.RTCVideoViewObjectFitCover
                : RTCVideoViewObjectFit.RTCVideoViewObjectFitContain,
          ),
        if (!visible || !_hasFrame) widget.placeholder,
      ],
    );
  }
}
