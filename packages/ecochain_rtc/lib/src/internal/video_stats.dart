import 'package:flutter_webrtc/flutter_webrtc.dart';

/// A measured receive sample. Missing platform fields stay unknown.
class VideoDebugSample {
  /// Creates a sample with unknown values omitted.
  const VideoDebugSample({this.codec, this.width, this.height, this.fps, this.kbps});

  /// Primary video codec, excluding repair payloads.
  final String? codec;

  /// Decoded frame width in pixels.
  final int? width;

  /// Decoded frame height in pixels.
  final int? height;

  /// Decoded frames per second, or the reported rate on the first sample.
  final double? fps;

  /// Received video payload bitrate in kilobits per second.
  final double? kbps;
}

/// Interprets stats selected for one receiver, using a monotonic local clock.
/// Native getStats timestamps are microseconds, while browsers use milliseconds;
/// elapsed local time avoids mixing these units across Flutter platforms.
class VideoStatsSampler {
  ({String id, Duration at, int? bytes, int? frames})? _previous;

  /// Drops the rate baseline when a receiver disappears or changes.
  void reset() => _previous = null;

  /// Reads a single matching video, without aggregating unrelated tracks.
  VideoDebugSample? sample(List<StatsReport> reports, {required Duration at, String? mediaTrackId}) {
    final byId = {for (final report in reports) report.id: report};
    String? codecOf(StatsReport report) {
      final mime = byId[report.values['codecId']]?.values['mimeType'];
      return mime is String ? mime.split('/').last.toUpperCase() : null;
    }

    String? trackOf(StatsReport report) {
      final direct = report.values['trackIdentifier'];
      final referenced = byId[report.values['trackId']]?.values['trackIdentifier'];
      return direct is String
          ? direct
          : referenced is String
          ? referenced
          : null;
    }

    final videos = reports.where((report) {
      if (report.type != 'inbound-rtp') return false;
      final values = report.values;
      if (values['kind'] != 'video' && values['mediaType'] != 'video') return false;
      return !const {'RTX', 'RED', 'ULPFEC', 'FLEXFEC-03'}.contains(codecOf(report));
    }).toList();
    final exact = mediaTrackId == null ? <StatsReport>[] : videos.where((r) => trackOf(r) == mediaTrackId).toList();
    // Native stats selectors may omit trackIdentifier. Accept that only for
    // an unambiguous receiver result, never by summing unrelated videos.
    final eligible = exact.isNotEmpty
        ? exact
        : videos.length == 1 && (mediaTrackId == null || trackOf(videos.single) == null)
        ? videos
        : <StatsReport>[];
    if (eligible.length != 1) {
      reset();
      return null;
    }
    final report = eligible.single;
    final values = report.values;
    final bytes = _number(values['bytesReceived'])?.toInt();
    final frames = _number(values['framesDecoded'])?.toInt();
    var fps = _number(values['framesPerSecond']);
    double? kbps;
    final previous = _previous;
    if (previous != null && previous.id == report.id && at > previous.at) {
      final seconds = (at - previous.at).inMicroseconds / Duration.microsecondsPerSecond;
      if (bytes != null && previous.bytes != null && bytes >= previous.bytes!) {
        kbps = (bytes - previous.bytes!) * 8 / (seconds * 1000);
      }
      if (frames != null && previous.frames != null) {
        fps = frames >= previous.frames! ? (frames - previous.frames!) / seconds : null;
      }
    }
    _previous = (id: report.id, at: at, bytes: bytes, frames: frames);
    final width = _number(values['frameWidth'])?.toInt();
    final height = _number(values['frameHeight'])?.toInt();
    return VideoDebugSample(
      codec: codecOf(report),
      width: width != null && width > 0 ? width : null,
      height: height != null && height > 0 ? height : null,
      fps: fps,
      kbps: kbps,
    );
  }

  static double? _number(Object? value) {
    final number = value is num
        ? value.toDouble()
        : value is String
        ? double.tryParse(value)
        : null;
    return number != null && number.isFinite && number >= 0 ? number : null;
  }
}
