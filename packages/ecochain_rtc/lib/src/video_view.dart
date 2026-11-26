import 'dart:async';
import 'dart:ui' show ImageFilter;

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
/// The whole frame is visible by default: it is fitted inside the area and the
/// space left around it is filled with a blurred copy of the same video.
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
    this.cover = false,
    this.placeholder = const SizedBox.shrink(),
  });

  /// Track to render. `null` or an audio track shows [placeholder].
  final RtcTrack? track;

  /// Mirror horizontally; use for the local front camera.
  final bool mirror;

  /// Fill the area by cropping the frame instead of showing all of it.
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

  Widget _video({required bool cover}) => RTCVideoView(
    _renderer,
    mirror: widget.mirror,
    objectFit: cover
        ? RTCVideoViewObjectFit.RTCVideoViewObjectFitCover
        : RTCVideoViewObjectFit.RTCVideoViewObjectFitContain,
  );

  /// A dimmed, blurred copy of the video behind the fitted one. It shares the
  /// renderer, so nothing is decoded twice, and is left out when the frame
  /// already fills the area.
  Widget _blurredFill() => LayoutBuilder(
    builder: (context, constraints) => ValueListenableBuilder<RTCVideoValue>(
      valueListenable: _renderer,
      builder: (context, value, child) {
        final area = constraints.biggest;
        if (area.isEmpty || (area.aspectRatio / value.aspectRatio - 1).abs() < 0.01) {
          return const SizedBox.shrink();
        }
        return child!;
      },
      child: ClipRect(
        child: Stack(
          fit: StackFit.expand,
          children: [
            ImageFiltered(
              imageFilter: ImageFilter.blur(sigmaX: 20, sigmaY: 20, tileMode: TileMode.mirror),
              child: _video(cover: true),
            ),
            const ColoredBox(color: Color(0x40000000)),
          ],
        ),
      ),
    ),
  );

  @override
  Widget build(BuildContext context) {
    final track = widget.track;
    final visible = _ready && track != null && track.canRender;
    return Stack(
      fit: StackFit.expand,
      children: [
        if (visible && !widget.cover) _blurredFill(),
        if (visible) _video(cover: widget.cover),
        if (!visible || !_hasFrame) widget.placeholder,
      ],
    );
  }
}
