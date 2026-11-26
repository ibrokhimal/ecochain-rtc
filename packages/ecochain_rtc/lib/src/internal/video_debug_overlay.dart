import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../participant.dart';
import 'video_stats.dart';

/// A per-tile debug view; it never owns the media track or the peer connection.
class VideoDebugOverlay extends StatefulWidget {
  /// Creates the debug label for one local preview or remote video.
  const VideoDebugOverlay({super.key, required this.track, this.clock});

  /// Track whose selected receiver statistics are displayed.
  final RtcTrack track;

  /// Replaces the monotonic clock for deterministic polling tests.
  @visibleForTesting
  final Duration Function()? clock;

  @override
  State<VideoDebugOverlay> createState() => _VideoDebugOverlayState();
}

class _VideoDebugOverlayState extends State<VideoDebugOverlay> {
  final _sampler = VideoStatsSampler();
  final _stopwatch = Stopwatch()..start();
  Timer? _timer;
  VideoDebugSample? _sample;
  var _generation = 0;
  var _reading = false;

  @override
  void initState() {
    super.initState();
    _restart();
  }

  @override
  void didUpdateWidget(VideoDebugOverlay oldWidget) {
    super.didUpdateWidget(oldWidget);
    final old = oldWidget.track, next = widget.track;
    if (old.id != next.id ||
        old.mediaTrack != next.mediaTrack ||
        old.isLocal != next.isLocal ||
        old.muted != next.muted ||
        (old.readStats == null) != (next.readStats == null)) {
      _restart();
    }
  }

  void _restart() {
    _generation++;
    _timer?.cancel();
    _timer = null;
    _reading = false;
    _sample = null;
    _sampler.reset();
    if (!kDebugMode ||
        widget.track.isLocal ||
        !widget.track.isVideo ||
        widget.track.muted ||
        widget.track.readStats == null) {
      return;
    }
    unawaited(_read());
    _timer = Timer.periodic(const Duration(seconds: 1), (_) => unawaited(_read()));
  }

  Future<void> _read() async {
    if (_reading) return;
    final reader = widget.track.readStats;
    if (reader == null) return;
    final generation = _generation;
    _reading = true;
    try {
      final reports = await reader();
      if (!mounted || generation != _generation) return;
      final sample = _sampler.sample(
        reports,
        at: widget.clock?.call() ?? _stopwatch.elapsed,
        mediaTrackId: widget.track.mediaTrack?.id,
      );
      setState(() => _sample = sample);
    } catch (_) {
      // A disappearing/replaced peer must not affect video rendering.
      if (!mounted || generation != _generation) return;
      _sampler.reset();
      setState(() => _sample = null);
    } finally {
      if (generation == _generation) _reading = false;
    }
  }

  @override
  void dispose() {
    _generation++;
    _timer?.cancel();
    _stopwatch.stop();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (!kDebugMode || !widget.track.isVideo || widget.track.muted) return const SizedBox.shrink();
    final sample = _sample;
    final size = sample?.width != null && sample?.height != null ? '${sample!.width}×${sample.height}' : '—';
    final label = widget.track.isLocal
        ? 'LOCAL'
        : 'RX • ${sample?.codec ?? '—'} • $size\n${sample?.fps?.toStringAsFixed(0) ?? '—'} FPS • ${sample?.kbps?.toStringAsFixed(0) ?? '—'} kbit/s';
    return IgnorePointer(
      child: Align(
        alignment: Alignment.topLeft,
        child: DecoratedBox(
          decoration: BoxDecoration(color: const Color(0xc0000000), borderRadius: BorderRadius.circular(4)),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
            child: Text(
              label,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(color: Color(0xffffffff), fontSize: 11, height: 1.25, fontFamily: 'monospace'),
            ),
          ),
        ),
      ),
    );
  }
}
